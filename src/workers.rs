use photopinfix::{
    map::{MapPoint, MapRenderer, MapScale, MapView},
    metadata::{MetadataTool, ScanResult},
    model::Candidate,
    preview,
};
use ratatui::layout::Size;
use ratatui_image::{
    Resize,
    picker::{Picker, ProtocolType},
    protocol::{Protocol, halfblocks::Halfblocks},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

pub enum Response {
    Scan(u64, Result<ScanResult, String>),
    Photo(u64, Result<Protocol, String>),
    Map(
        u64,
        Result<Protocol, String>,
        String,
        String,
        Option<MapScale>,
    ),
    Written(u64, usize, Candidate, Result<(), String>),
    WriteFinished(u64, usize, usize),
}

pub struct PhotoRequest {
    pub id: u64,
    pub path: PathBuf,
    pub size: Size,
}
pub struct MapRequest {
    pub id: u64,
    pub points: Vec<MapPoint>,
    pub view: MapView,
    pub size: Size,
    pub background: bool,
    pub source: usize,
    pub cancel: Arc<AtomicBool>,
}

pub struct Workers {
    pub tx: Sender<Response>,
    pub rx: Receiver<Response>,
    photo: Option<Sender<PhotoRequest>>,
    map: Option<Sender<MapRequest>>,
    io: Arc<IoGate>,
    threads: Vec<JoinHandle<()>>,
    map_cancel: Option<Arc<AtomicBool>>,
}

/// All reads and writes share this gate because restoring a file's access time
/// must not race another operation on that file. Stop is checked under the lock,
/// so waiting operations cannot start after shutdown has been requested.
#[derive(Default)]
struct IoGate {
    lock: Mutex<()>,
    stopping: AtomicBool,
}

impl IoGate {
    fn run<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.stopping.load(Ordering::Acquire) {
            None
        } else {
            Some(operation())
        }
    }
}

impl Workers {
    pub fn new(tool: MetadataTool, picker: Picker) -> Self {
        let (tx, rx) = mpsc::channel();
        let (photo_tx, photo_rx) = mpsc::channel::<PhotoRequest>();
        let (map_tx, map_rx) = mpsc::channel::<MapRequest>();
        let io = Arc::new(IoGate::default());
        let photo_io = io.clone();
        let photo_results = tx.clone();
        let photo_picker = picker.clone();
        let photo_thread = thread::spawn(move || {
            while let Ok(mut request) = photo_rx.recv() {
                while let Ok(newer) = photo_rx.try_recv() {
                    request = newer;
                }
                let pixels = photo_picker.font_size();
                let Some(result) = photo_io.run(|| {
                    preview::load_preview(
                        &request.path,
                        &tool,
                        (request.size.width as u32 * pixels.width as u32).min(2400),
                        (request.size.height as u32 * pixels.height as u32).min(1600),
                    )
                }) else {
                    break;
                };
                if photo_io.stopping.load(Ordering::Acquire) {
                    break;
                }
                let result = result.and_then(|image| {
                    photo_picker
                        .new_protocol(image, request.size, Resize::Fit(None))
                        .map_err(|e| e.to_string())
                });
                if photo_results
                    .send(Response::Photo(request.id, result))
                    .is_err()
                {
                    break;
                }
            }
        });
        let map_results = tx.clone();
        let map_io = io.clone();
        let map_thread = thread::spawn(move || {
            let mut renderer = MapRenderer::new();
            let picker = picker;
            let halfblocks = picker.protocol_type() == ProtocolType::Halfblocks;
            if halfblocks {
                renderer.set_marker_radii(1, 2, 1);
            }
            while let Ok(mut request) = map_rx.recv() {
                while let Ok(newer) = map_rx.try_recv() {
                    request = newer;
                }
                if map_io.stopping.load(Ordering::Acquire) {
                    break;
                }
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let font = picker.font_size();
                let (width, height) = if halfblocks {
                    // A half-block cell represents exactly two image pixels.
                    // Rendering at font resolution would shrink point markers
                    // until normal photos became almost invisible.
                    (request.size.width as u32, request.size.height as u32 * 2)
                } else {
                    (
                        (request.size.width as u32 * font.width as u32).clamp(1, 2400),
                        (request.size.height as u32 * font.height as u32).clamp(1, 1600),
                    )
                };
                let result = renderer.render_cancellable(
                    &request.points,
                    &request.view,
                    width,
                    height,
                    request.background,
                    request.source,
                    &request.cancel,
                );
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let image_width = result.image.width();
                let image_height = result.image.height();
                let protocol = if halfblocks {
                    Halfblocks::new(result.image, request.size).map(Protocol::Halfblocks)
                } else {
                    picker.new_protocol(result.image, request.size, Resize::Fit(None))
                }
                .map_err(|e| e.to_string());
                let scale = protocol.as_ref().ok().and_then(|protocol| {
                    request
                        .view
                        .scale(image_width, image_height, protocol.size().width)
                });
                if map_results
                    .send(Response::Map(
                        request.id,
                        protocol,
                        result.status,
                        result.attribution,
                        scale,
                    ))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            tx,
            rx,
            photo: Some(photo_tx),
            map: Some(map_tx),
            io,
            threads: vec![photo_thread, map_thread],
            map_cancel: None,
        }
    }

    pub fn request_photo(&self, request: PhotoRequest) {
        if let Some(sender) = &self.photo {
            let _ = sender.send(request);
        }
    }

    pub fn request_map(&mut self, request: MapRequest) {
        if let Some(previous) = self.map_cancel.replace(request.cancel.clone()) {
            previous.store(true, Ordering::Relaxed);
        }
        if let Some(sender) = &self.map {
            let _ = sender.send(request);
        }
    }

    pub fn scan(&mut self, generation: u64, tool: MetadataTool, path: PathBuf) {
        let tx = self.tx.clone();
        let io = self.io.clone();
        self.reap_finished();
        self.threads.push(thread::spawn(move || {
            if let Some(result) = io.run(|| tool.scan_directory(&path)) {
                let _ = tx.send(Response::Scan(generation, result));
            }
        }));
    }

    pub fn write(
        &mut self,
        generation: u64,
        tool: MetadataTool,
        items: Vec<(usize, PathBuf, Candidate)>,
    ) {
        let tx = self.tx.clone();
        let io = self.io.clone();
        self.reap_finished();
        self.threads.push(thread::spawn(move || {
            // A confirmed write batch must finish even if terminal I/O fails.
            let _guard = io.lock.lock().unwrap_or_else(|e| e.into_inner());
            let total = items.len();
            let mut success = 0;
            for (index, path, candidate) in items {
                let result = tool.write_gps(&path, &candidate);
                if result.is_ok() {
                    success += 1;
                }
                if tx
                    .send(Response::Written(generation, index, candidate, result))
                    .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(Response::WriteFinished(generation, success, total));
        }));
    }

    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.threads.len() {
            if self.threads[index].is_finished() {
                let _ = self.threads.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }

    fn stop_requests(&mut self) {
        self.io.stopping.store(true, Ordering::Release);
        if let Some(cancel) = self.map_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        // Closing channels wakes idle workers; the stop flag discards queued
        // requests. Joining active work lets timestamp guards restore atime.
        self.photo.take();
        self.map.take();
    }

    pub fn shutdown(&mut self) {
        self.stop_requests();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn shutdown_waits_for_active_read_cleanup_and_skips_queued_reads() {
        struct RestoreOnDrop(Arc<AtomicBool>);
        impl Drop for RestoreOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let io = Arc::new(IoGate::default());
        let restored = Arc::new(AtomicBool::new(false));
        let queued_read_started = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active_io = io.clone();
        let active_restored = restored.clone();
        let active = thread::spawn(move || {
            active_io.run(|| {
                let _guard = RestoreOnDrop(active_restored);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let queued_io = io.clone();
        let queued_started = queued_read_started.clone();
        let queued = thread::spawn(move || {
            queued_io.run(|| queued_started.store(true, Ordering::Release));
        });
        let (tx, rx) = mpsc::channel();
        let map_cancel = Arc::new(AtomicBool::new(false));
        let mut workers = Workers {
            tx,
            rx,
            photo: None,
            map: None,
            io,
            threads: vec![active, queued],
            map_cancel: Some(map_cancel.clone()),
        };
        // Stop requests first so the test can deterministically release active
        // I/O after cancellation, independently of thread scheduling.
        workers.stop_requests();
        assert!(map_cancel.load(Ordering::Relaxed));
        let (finished_tx, finished_rx) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            workers.shutdown();
            finished_tx.send(()).unwrap();
        });
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!restored.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        shutdown.join().unwrap();
        assert!(restored.load(Ordering::Acquire));
        assert!(!queued_read_started.load(Ordering::Acquire));
    }
}
