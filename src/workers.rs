use photopinfix::{
    map::{self, MapPoint, MapRenderer, MapResult, MapScale, MapView},
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
    Map(u64, MapResult),
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
    pub view: MapView,
    pub size: Size,
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
    map_picker: Picker,
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
        let map_picker = picker.clone();
        let map_results = tx.clone();
        let map_io = io.clone();
        let map_thread = thread::spawn(move || {
            let mut renderer = MapRenderer::new();
            let picker = picker;
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
                let [width, height] = map_dimensions(&picker, request.size);
                let result = renderer.render_background_cancellable(
                    &request.view,
                    width,
                    height,
                    request.source,
                    &request.cancel,
                );
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                if map_results.send(Response::Map(request.id, result)).is_err() {
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
            map_picker,
        }
    }

    pub fn map_dimensions(&self, size: Size) -> [u32; 2] {
        map_dimensions(&self.map_picker, size)
    }

    /// This path performs no I/O: current markers are composited immediately,
    /// independently of an outstanding network request for background tiles.
    pub fn compose_map(
        &self,
        background: Option<&image::DynamicImage>,
        points: &[MapPoint],
        view: &MapView,
        size: Size,
    ) -> (Result<Protocol, String>, Option<MapScale>) {
        let dimensions = self.map_dimensions(size);
        let halfblocks = self.map_picker.protocol_type() == ProtocolType::Halfblocks;
        let radii = if halfblocks { (1, 2, 1) } else { (3, 7, 2) };
        let image = map::compose_points(background, points, view, dimensions, radii);
        let protocol = if halfblocks {
            Halfblocks::new(image, size).map(Protocol::Halfblocks)
        } else {
            self.map_picker.new_protocol(image, size, Resize::Fit(None))
        }
        .map_err(|error| error.to_string());
        let scale = protocol
            .as_ref()
            .ok()
            .and_then(|protocol| view.scale(dimensions[0], dimensions[1], protocol.size().width));
        (protocol, scale)
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

fn map_dimensions(picker: &Picker, size: Size) -> [u32; 2] {
    if picker.protocol_type() == ProtocolType::Halfblocks {
        [
            u32::from(size.width).clamp(1, 2048),
            (u32::from(size.height) * 2).clamp(1, 1536),
        ]
    } else {
        let font = picker.font_size();
        [
            (u32::from(size.width) * u32::from(font.width)).clamp(1, 2048),
            (u32::from(size.height) * u32::from(font.height)).clamp(1, 1536),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photopinfix::model::Coordinates;
    use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};
    use std::time::Duration;

    #[test]
    fn terminal_points_refresh_while_background_requests_are_still_pending() {
        let (tx, rx) = mpsc::channel();
        let (map_tx, map_rx) = mpsc::channel();
        let mut workers = Workers {
            tx,
            rx,
            photo: None,
            map: Some(map_tx),
            io: Arc::new(IoGate::default()),
            threads: Vec::new(),
            map_cancel: None,
            map_picker: Picker::halfblocks(),
        };
        let old_point = MapPoint {
            coordinates: Coordinates::new(30.0, 120.0).unwrap(),
            selected: true,
            existing: true,
        };
        let new_point = MapPoint {
            coordinates: Coordinates::new(30.02, 120.02).unwrap(),
            selected: true,
            existing: false,
        };
        let view = MapView::fit(&[old_point, new_point]);
        let size = Size::new(80, 30);
        let old_cancel = Arc::new(AtomicBool::new(false));
        workers.request_map(MapRequest {
            id: 1,
            view: view.clone(),
            size,
            source: 0,
            cancel: old_cancel.clone(),
        });
        let held_request = map_rx.recv().unwrap();
        let (old_image, _) = workers.compose_map(None, &[old_point], &view, size);
        workers.request_map(MapRequest {
            id: 2,
            view: view.clone(),
            size,
            source: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        assert!(held_request.cancel.load(Ordering::Relaxed));
        assert!(old_cancel.load(Ordering::Relaxed));
        assert_eq!(map_rx.recv().unwrap().id, 2);
        // Neither request has a response. The current terminal image and metric
        // ruler must already exist, and old-session green dots must be gone.
        let (new_image, scale) = workers.compose_map(None, &[new_point], &view, size);
        assert!(scale.is_some());
        let rect = Rect::new(0, 0, size.width, size.height);
        let mut before = Buffer::empty(rect);
        let mut after = Buffer::empty(rect);
        ratatui_image::Image::new(&old_image.unwrap()).render(rect, &mut before);
        ratatui_image::Image::new(&new_image.unwrap()).render(rect, &mut after);
        let contains = |buffer: &Buffer, color| {
            buffer
                .content()
                .iter()
                .any(|cell| cell.fg == color || cell.bg == color)
        };
        assert!(contains(&before, Color::Rgb(76, 220, 128)));
        assert!(!contains(&after, Color::Rgb(76, 220, 128)));
        assert!(contains(&after, Color::Rgb(255, 209, 76)));
        assert!(workers.rx.try_recv().is_err());
    }

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
            map_picker: Picker::halfblocks(),
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
