//! Desktop-independent state. All photo I/O is serialized on one worker, so
//! metadata writes and access-time restoration cannot race preview reads.
use image::DynamicImage;
use photopinfix::{
    coordinates::CoordinateFormat,
    map::MapPoint,
    metadata::{MetadataTool, ScanResult},
    model::{
        Candidate, Coordinates, Method, Mode, Photo, PhotoStats, Provenance, Session, Settings,
    },
    preview, strategy,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

#[derive(Clone, Debug, PartialEq)]
pub struct WriteItem {
    pub index: usize,
    pub path: PathBuf,
    pub candidate: Candidate,
}

/// The confirmation dialog renders this exact snapshot. Later selection or
/// session changes do not silently expand the batch the user has reviewed.
#[derive(Clone, Debug)]
pub struct WriteSnapshot {
    pub items: Vec<WriteItem>,
    pub scope: String,
    directory_generation: u64,
}

pub struct GuiState {
    pub directory: PathBuf,
    pub photos: Vec<Photo>,
    pub directories: Vec<PathBuf>,
    pub sessions: Vec<Session>,
    /// None displays all sessions; selected is always a global photo index.
    pub session: Option<usize>,
    pub selected: Option<usize>,
    pub settings: Settings,
    pub coordinate_format: CoordinateFormat,
    pub status: String,
    pub warnings: Vec<String>,
    pub loading: bool,
    pub writing: bool,
    pub write_progress: (usize, usize),
    /// Changes when the list, scope, selection or effective coordinates change.
    pub revision: u64,
    pub preview: Option<DynamicImage>,
    pub preview_status: String,
    /// Changes when the displayed image is cleared or replaced.
    pub preview_revision: u64,
    workers: IoWorker,
    directory_generation: u64,
    preview_generation: u64,
    last_session: usize,
    preview_size: (u32, u32),
    preview_dirty: bool,
    scan_cancel: Arc<AtomicBool>,
    preview_cancel: Arc<AtomicBool>,
    closed: bool,
}

impl GuiState {
    /// Prepare an empty desktop without scanning until a folder is chosen.
    pub fn idle(directory: PathBuf, tool: MetadataTool) -> Self {
        let mut state = Self::empty(directory, IoWorker::new(tool));
        state.status = "选择照片文件夹开始".into();
        state
    }

    pub fn new(directory: PathBuf, tool: MetadataTool) -> Self {
        let mut state = Self::empty(directory, IoWorker::new(tool));
        state.start_scan();
        state
    }

    fn empty(directory: PathBuf, workers: IoWorker) -> Self {
        Self {
            directory,
            photos: Vec::new(),
            directories: Vec::new(),
            sessions: Vec::new(),
            session: Some(0),
            selected: None,
            settings: Settings::default(),
            coordinate_format: CoordinateFormat::default(),
            status: String::new(),
            warnings: Vec::new(),
            loading: false,
            writing: false,
            write_progress: (0, 0),
            revision: 0,
            preview: None,
            preview_status: String::new(),
            preview_revision: 0,
            workers,
            directory_generation: 0,
            preview_generation: 0,
            last_session: 0,
            preview_size: (0, 0),
            preview_dirty: true,
            scan_cancel: Arc::new(AtomicBool::new(false)),
            preview_cancel: Arc::new(AtomicBool::new(false)),
            closed: false,
        }
    }

    pub fn visible_indices(&self) -> Vec<usize> {
        match self.session {
            Some(index) => self
                .sessions
                .get(index)
                .map(|s| s.indices.clone())
                .unwrap_or_default(),
            None => (0..self.photos.len()).collect(),
        }
    }

    pub fn selected_photo(&self) -> Option<&Photo> {
        self.selected.and_then(|index| self.photos.get(index))
    }

    pub fn stats(&self) -> PhotoStats {
        PhotoStats::from_photos(
            self.visible_indices()
                .iter()
                .map(|index| &self.photos[*index]),
        )
    }

    /// Nearest still-unwritten GPS gap in this scope. Does not wrap at an edge;
    /// with no selection, forward starts at the first gap and backward at last.
    pub fn missing_neighbor(&self, delta: isize) -> Option<usize> {
        if delta == 0 {
            return None;
        }
        let visible = self.visible_indices();
        let current = self
            .selected
            .and_then(|index| visible.iter().position(|item| *item == index));
        let eligible = |(position, index): &(usize, &usize)| {
            self.photos[**index].status().is_missing()
                && current.is_none_or(|current| {
                    if delta < 0 {
                        *position < current
                    } else {
                        *position > current
                    }
                })
        };
        if delta < 0 {
            visible
                .iter()
                .enumerate()
                .rev()
                .find(eligible)
                .map(|(_, index)| *index)
        } else {
            visible
                .iter()
                .enumerate()
                .find(eligible)
                .map(|(_, index)| *index)
        }
    }

    pub fn previous_missing(&mut self) -> bool {
        self.navigate_missing(-1)
    }

    pub fn next_missing(&mut self) -> bool {
        self.navigate_missing(1)
    }

    fn navigate_missing(&mut self, delta: isize) -> bool {
        if let Some(index) = self.missing_neighbor(delta) {
            return self.select(index);
        }
        self.status = if delta < 0 {
            "当前范围前方没有缺少 GPS 的照片"
        } else {
            "当前范围后方没有缺少 GPS 的照片"
        }
        .into();
        false
    }

    pub fn map_points(&self) -> Vec<MapPoint> {
        self.visible_indices()
            .into_iter()
            .filter_map(|index| {
                let photo = &self.photos[index];
                photo.effective_coordinates().map(|coordinates| MapPoint {
                    coordinates,
                    selected: self.selected == Some(index),
                    existing: photo.gps.is_some(),
                })
            })
            .collect()
    }

    pub fn scope_label(&self) -> String {
        match self.session {
            Some(index) if !self.sessions.is_empty() => {
                format!("拍摄 {}/{}", index + 1, self.sessions.len())
            }
            Some(_) => "暂无拍摄".into(),
            None => "全部照片".into(),
        }
    }

    pub fn title(&self) -> String {
        let visible = self.visible_indices();
        let first = visible.first().and_then(|i| self.photos.get(*i));
        let last = visible.last().and_then(|i| self.photos.get(*i));
        let range = match (first, last) {
            (Some(first), Some(last)) => format!(
                "{} — {}",
                first.capture_time_label(),
                last.capture_time_label()
            ),
            _ => "暂无照片".into(),
        };
        format!(
            "PhotoPinFix · {} · {range} · {} 张",
            self.scope_label(),
            visible.len()
        )
    }

    pub fn has_manual_candidates(&self) -> bool {
        self.photos.iter().any(|p| {
            p.gps.is_none()
                && p.candidate
                    .is_some_and(|c| c.provenance.mode == Mode::Manual)
        })
    }

    /// The UI must confirm discarding has_manual_candidates() before invoking
    /// directory navigation or rescan; this layer performs the authorized action.
    pub fn open_directory(&mut self, path: PathBuf) -> Result<(), String> {
        self.not_writing()?;
        let path = if path.is_absolute() {
            path
        } else {
            self.directory.join(path)
        };
        if !path.is_dir() {
            return Err(format!("目标不是目录：{}", path.display()));
        }
        self.directory = path;
        self.start_scan();
        Ok(())
    }

    pub fn rescan(&mut self) -> Result<(), String> {
        self.not_writing()?;
        self.start_scan();
        Ok(())
    }

    fn start_scan(&mut self) {
        self.scan_cancel.store(true, Ordering::Release);
        self.scan_cancel = Arc::new(AtomicBool::new(false));
        self.directory_generation += 1;
        self.loading = true;
        self.photos.clear();
        self.directories.clear();
        self.sessions.clear();
        self.warnings.clear();
        self.selected = None;
        self.session = Some(0);
        self.last_session = 0;
        self.invalidate_preview();
        self.revision += 1;
        self.status = format!("正在扫描 {}…", self.directory.display());
        if let Err(error) = self.workers.send(Request::Scan {
            generation: self.directory_generation,
            directory: self.directory.clone(),
            cancel: self.scan_cancel.clone(),
        }) {
            self.loading = false;
            self.status = error;
        }
    }

    pub fn select(&mut self, index: usize) -> bool {
        if self.selected == Some(index) || !self.visible_indices().contains(&index) {
            return false;
        }
        self.selected = Some(index);
        self.selection_changed();
        true
    }

    pub fn move_selection(&mut self, delta: isize) -> bool {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return false;
        }
        let current = visible
            .iter()
            .position(|i| Some(*i) == self.selected)
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(visible.len() - 1);
        self.select(visible[next])
    }

    pub fn switch_session(&mut self, delta: isize) -> bool {
        if self.sessions.is_empty() {
            return false;
        }
        let current = self.session.unwrap_or(self.last_session);
        let next = current
            .saturating_add_signed(delta)
            .min(self.sessions.len() - 1);
        if self.session == Some(next) {
            return false;
        }
        self.session = Some(next);
        self.last_session = next;
        self.select_first_visible();
        true
    }

    pub fn toggle_all(&mut self) -> bool {
        if self.sessions.is_empty() {
            return false;
        }
        self.session = match self.session {
            Some(index) => {
                self.last_session = index;
                None
            }
            None => Some(self.last_session.min(self.sessions.len() - 1)),
        };
        self.select_first_visible();
        true
    }

    fn select_first_visible(&mut self) {
        self.selected = self.visible_indices().first().copied();
        self.selection_changed();
    }

    fn selection_changed(&mut self) {
        self.revision += 1;
        self.invalidate_preview();
    }

    fn invalidate_preview(&mut self) {
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_cancel = Arc::new(AtomicBool::new(false));
        self.preview_generation += 1;
        self.preview_revision += 1;
        self.preview = None;
        self.preview_dirty = true;
        self.preview_status = if self.selected.is_some() {
            "正在读取照片…"
        } else {
            "选择照片以预览"
        }
        .into();
    }

    /// Call on layout changes (or every frame); unchanged dimensions are free.
    /// Capping decode dimensions avoids allocating full camera sensor images.
    pub fn request_preview(&mut self, width: u32, height: u32) {
        let size = (width.min(2400), height.min(1600));
        if size != self.preview_size {
            self.preview_size = size;
            self.invalidate_preview();
        }
        self.schedule_preview();
    }

    fn schedule_preview(&mut self) {
        if self.closed
            || self.loading
            || self.writing
            || !self.preview_dirty
            || self.preview_size.0 == 0
            || self.preview_size.1 == 0
        {
            return;
        }
        self.preview_dirty = false;
        let Some(photo) = self.selected_photo() else {
            return;
        };
        let path = photo.path.clone();
        if let Err(error) = self.workers.send(Request::Preview {
            generation: self.preview_generation,
            path,
            size: self.preview_size,
            cancel: self.preview_cancel.clone(),
        }) {
            self.preview_status = error;
        }
    }

    pub fn apply_settings(
        &mut self,
        settings: Settings,
        format: CoordinateFormat,
    ) -> Result<(), String> {
        self.editable()?;
        self.settings = settings.validate()?;
        self.coordinate_format = format;
        strategy::recompute_candidates(&mut self.photos, self.settings);
        self.revision += 1;
        self.status = "设置已应用，仅本次运行有效；手动候选已保留".into();
        Ok(())
    }

    pub fn manual(&mut self, method: Method) -> Result<(), String> {
        self.editable()?;
        let index = self.selected.ok_or("未选中照片")?;
        let candidate = strategy::manual_candidate(&self.photos, index, method)?;
        self.photos[index].candidate = Some(candidate);
        self.revision += 1;
        self.status = format!("已暂存{}；确认后写入", method.label());
        Ok(())
    }

    pub fn stage_coordinates(&mut self, coordinates: Coordinates) -> Result<(), String> {
        self.editable()?;
        let coordinates = Coordinates::new(coordinates.lat, coordinates.lon)?;
        let photo = self
            .selected
            .and_then(|i| self.photos.get_mut(i))
            .ok_or("未选中照片")?;
        if photo.gps.is_some() {
            return Err("已有 GPS，跳过修改".into());
        }
        photo.candidate = Some(Candidate {
            coordinates,
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
        });
        self.revision += 1;
        self.status = "手动坐标已暂存；确认后写入".into();
        Ok(())
    }

    pub fn restore_auto(&mut self) -> Result<(), String> {
        self.editable()?;
        let photo = self
            .selected
            .and_then(|i| self.photos.get_mut(i))
            .ok_or("未选中照片")?;
        if photo.gps.is_some() {
            return Err("已有 GPS，跳过修改".into());
        }
        photo.candidate = None;
        strategy::recompute_candidates(&mut self.photos, self.settings);
        self.revision += 1;
        self.status = "已恢复自动建议".into();
        Ok(())
    }

    pub fn write_snapshot(&self, all: bool) -> Result<WriteSnapshot, String> {
        self.editable()?;
        let indices = if all {
            self.visible_indices()
        } else {
            self.selected.into_iter().collect()
        };
        let items: Vec<_> = indices
            .into_iter()
            .filter_map(|index| {
                let photo = self.photos.get(index)?;
                if photo.gps.is_some() {
                    return None;
                }
                photo.candidate.map(|candidate| WriteItem {
                    index,
                    path: photo.path.clone(),
                    candidate,
                })
            })
            .collect();
        if items.is_empty() {
            return Err("没有可写入的 GPS 候选；已有 GPS 的文件会跳过".into());
        }
        Ok(WriteSnapshot {
            items,
            scope: self.scope_label(),
            directory_generation: self.directory_generation,
        })
    }

    pub fn confirm_write(&mut self, snapshot: WriteSnapshot) -> Result<(), String> {
        self.validate_snapshot(&snapshot)?;
        let total = snapshot.items.len();
        self.workers.send(Request::Write {
            generation: self.directory_generation,
            items: snapshot.items,
        })?;
        self.writing = true;
        self.write_progress = (0, total);
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_dirty = true;
        self.status = format!("正在写入并校验 {total} 张照片…");
        Ok(())
    }

    fn validate_snapshot(&self, snapshot: &WriteSnapshot) -> Result<(), String> {
        self.editable()?;
        if snapshot.directory_generation != self.directory_generation || snapshot.items.is_empty() {
            return Err("待写入目录已变化，请重新确认".into());
        }
        for item in &snapshot.items {
            if !self.photos.get(item.index).is_some_and(|photo| {
                photo.path == item.path
                    && photo.gps.is_none()
                    && photo.candidate == Some(item.candidate)
            }) {
                return Err("候选坐标已变化，请重新查看并确认".into());
            }
        }
        Ok(())
    }

    fn not_writing(&self) -> Result<(), String> {
        if self.closed {
            Err("窗口正在关闭".into())
        } else if self.writing {
            Err("正在写入并校验，请稍候…".into())
        } else {
            Ok(())
        }
    }

    fn editable(&self) -> Result<(), String> {
        self.not_writing()?;
        if self.loading {
            Err("正在扫描照片，请稍候…".into())
        } else {
            Ok(())
        }
    }

    /// Apply completed background work without blocking the UI thread.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.workers.responses.try_recv() {
                Ok(response) => changed |= self.accept(response),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.closed && (self.loading || self.writing) {
                        self.loading = false;
                        self.writing = false;
                        self.status = "照片 I/O 线程已停止，请重新打开应用".into();
                        self.warnings.push(self.status.clone());
                        changed = true;
                    }
                    break;
                }
            }
        }
        self.schedule_preview();
        changed
    }

    fn accept(&mut self, response: Response) -> bool {
        match response {
            Response::Scan(generation, result) if generation == self.directory_generation => {
                self.loading = false;
                match result {
                    Ok(mut result) => {
                        strategy::sort_photos(&mut result.photos);
                        strategy::recompute_candidates(&mut result.photos, self.settings);
                        self.photos = result.photos;
                        self.directories = result.directories;
                        self.warnings = result.warnings;
                        self.sessions = strategy::build_sessions(&self.photos);
                        self.session = Some(0);
                        self.last_session = 0;
                        self.select_first_visible();
                        self.status = format!(
                            "找到 {} 张照片 · {} 次拍摄 · {} 条提示",
                            self.photos.len(),
                            self.sessions.len(),
                            self.warnings.len()
                        );
                    }
                    Err(error) => {
                        self.warnings.push(error.clone());
                        self.status = error;
                    }
                }
            }
            Response::Preview(generation, result) if generation == self.preview_generation => {
                self.preview_revision += 1;
                match result {
                    Ok(image) => {
                        self.preview = Some(image);
                        self.preview_status.clear();
                    }
                    Err(error) => {
                        self.preview = None;
                        self.preview_status = error;
                    }
                }
            }
            Response::Written(generation, item, result)
                if generation == self.directory_generation =>
            {
                if let Some(photo) = self
                    .photos
                    .get_mut(item.index)
                    .filter(|p| p.path == item.path)
                {
                    match result {
                        Ok(()) => {
                            photo.gps = Some(item.candidate.coordinates);
                            photo.provenance = Some(item.candidate.provenance);
                            photo.candidate = None;
                            photo.error = None;
                        }
                        Err(error) => {
                            self.warnings
                                .push(format!("{}: {error}", item.path.display()));
                            photo.error = Some(error);
                        }
                    }
                }
                self.write_progress.0 += 1;
                self.revision += 1;
            }
            Response::WriteFinished(generation, success, total)
                if generation == self.directory_generation =>
            {
                self.writing = false;
                self.write_progress = (total, total);
                self.status = format!("写入完成：成功 {success}/{total}；失败 {}", total - success);
                // Reset cancellation so an aborted queued preview is retried.
                self.invalidate_preview();
            }
            _ => return false,
        }
        true
    }

    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.scan_cancel.store(true, Ordering::Release);
        self.preview_cancel.store(true, Ordering::Release);
        self.workers.shutdown();
    }
}

impl Drop for GuiState {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum Request {
    Scan {
        generation: u64,
        directory: PathBuf,
        cancel: Arc<AtomicBool>,
    },
    Preview {
        generation: u64,
        path: PathBuf,
        size: (u32, u32),
        cancel: Arc<AtomicBool>,
    },
    Write {
        generation: u64,
        items: Vec<WriteItem>,
    },
}

enum Response {
    Scan(u64, Result<ScanResult, String>),
    Preview(u64, Result<DynamicImage, String>),
    Written(u64, WriteItem, Result<(), String>),
    WriteFinished(u64, usize, usize),
}

struct IoWorker {
    requests: Option<Sender<Request>>,
    responses: Receiver<Response>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl IoWorker {
    fn new(tool: MetadataTool) -> Self {
        let (requests, receiver) = mpsc::channel();
        let (sender, responses) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let thread = thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                match request {
                    Request::Scan {
                        generation,
                        directory,
                        cancel,
                    } => {
                        if stop.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
                            continue;
                        }
                        let result = guarded(|| tool.scan_directory(&directory));
                        let _ = sender.send(Response::Scan(generation, result));
                    }
                    Request::Preview {
                        generation,
                        path,
                        size,
                        cancel,
                    } => {
                        if stop.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
                            continue;
                        }
                        let result =
                            guarded(|| preview::load_preview(&path, &tool, size.0, size.1));
                        let _ = sender.send(Response::Preview(generation, result));
                    }
                    Request::Write { generation, items } => {
                        // A confirmed batch finishes even when the window is
                        // closing. Only unstarted reads can be discarded.
                        let total = items.len();
                        let mut success = 0;
                        for item in items {
                            let result = guarded(|| tool.write_gps(&item.path, &item.candidate));
                            success += usize::from(result.is_ok());
                            let _ = sender.send(Response::Written(generation, item, result));
                        }
                        let _ = sender.send(Response::WriteFinished(generation, success, total));
                    }
                }
            }
        });
        Self {
            requests: Some(requests),
            responses,
            stopping,
            thread: Some(thread),
        }
    }

    fn send(&self, request: Request) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or("照片 I/O 线程已关闭")?
            .send(request)
            .map_err(|_| "照片 I/O 线程已停止".into())
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for IoWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn guarded<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        Err(format!("照片处理失败：{message}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};
    use std::{
        fs::{self, FileTimes},
        path::Path,
        time::{Duration as StdDuration, SystemTime},
    };

    fn state() -> GuiState {
        let (requests, _) = mpsc::channel();
        let (_, responses) = mpsc::channel();
        GuiState::empty(
            "/photos".into(),
            IoWorker {
                requests: Some(requests),
                responses,
                stopping: Arc::new(AtomicBool::new(false)),
                thread: None,
            },
        )
    }

    fn photo(seconds: i64, gps: bool) -> Photo {
        Photo {
            path: PathBuf::from(format!("/photos/{seconds}.jpg")),
            captured_at: Some(
                NaiveDate::from_ymd_opt(2026, 11, 22)
                    .unwrap()
                    .and_hms_opt(10, 0, 0)
                    .unwrap()
                    + Duration::seconds(seconds),
            ),
            capture_time_info: Default::default(),
            gps: gps.then_some(Coordinates::new(30.0, 120.0).unwrap()),
            provenance: None,
            candidate: None,
            error: None,
        }
    }

    fn loaded() -> GuiState {
        let mut state = state();
        state.accept(Response::Scan(
            0,
            Ok(ScanResult {
                photos: vec![
                    photo(0, true),
                    photo(30, false),
                    photo(4000, true),
                    photo(4030, false),
                ],
                directories: Vec::new(),
                warnings: Vec::new(),
            }),
        ));
        state
    }

    #[test]
    fn scoped_stats_follow_manual_override_failures_and_successful_writes() {
        let mut state = loaded();
        assert_eq!(
            state.stats(),
            PhotoStats {
                total: 2,
                native_gps: 1,
                auto_pending: 1,
                ..Default::default()
            }
        );
        state.select(1);
        state
            .stage_coordinates(Coordinates::new(12.0, 34.0).unwrap())
            .unwrap();
        assert_eq!(
            (state.stats().auto_pending, state.stats().manual_pending),
            (0, 1)
        );
        let item = WriteItem {
            index: 1,
            path: state.photos[1].path.clone(),
            candidate: state.photos[1].candidate.unwrap(),
        };
        let before = state.stats();
        assert!(!state.accept(Response::Written(99, item.clone(), Ok(()))));
        assert_eq!(state.stats(), before);
        state.accept(Response::Written(
            0,
            item.clone(),
            Err("write failed".into()),
        ));
        assert_eq!(state.stats(), before);
        assert!(state.photos[1].gps.is_none());
        state.accept(Response::Written(0, item, Ok(())));
        let after = state.stats();
        assert_eq!(
            (after.written, after.manual_pending, after.pending_write()),
            (1, 0, 0)
        );
        assert_eq!(
            (after.missing_total(), after.assigned()),
            (before.missing_total(), before.assigned())
        );
        assert_eq!(state.missing_neighbor(1), None);
        state.toggle_all();
        assert_eq!(
            (
                state.stats().total,
                state.stats().native_gps,
                state.stats().written,
                state.stats().auto_pending
            ),
            (4, 2, 1, 1)
        );
    }

    #[test]
    fn missing_navigation_includes_all_drafts_stops_at_edges_and_stays_in_scope() {
        let mut state = state();
        state.photos = vec![
            photo(0, true),
            photo(30, false),
            photo(90, false),
            photo(120, false),
            photo(150, true),
            photo(4000, false),
        ];
        let candidate = Candidate {
            coordinates: Coordinates::new(10.0, 20.0).unwrap(),
            provenance: Provenance {
                mode: Mode::Auto,
                method: Method::CopyPrevious,
            },
        };
        state.photos[1].candidate = Some(candidate);
        state.photos[3].candidate = Some(Candidate {
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
            ..candidate
        });
        state.photos[4].provenance = Some(candidate.provenance);
        state.sessions = strategy::build_sessions(&state.photos);
        state.selected = Some(0);
        assert_eq!(state.missing_neighbor(-1), None);
        for index in [1, 2, 3] {
            assert!(state.next_missing());
            assert_eq!(state.selected, Some(index));
        }
        assert!(!state.next_missing());
        assert_eq!(state.selected, Some(3));
        assert!(state.previous_missing());
        assert_eq!(state.selected, Some(2));
        assert_eq!(state.stats().missing_total(), 4);
        state.toggle_all();
        state.select(3);
        assert_eq!(state.missing_neighbor(1), Some(5));
        assert!(state.next_missing());
        assert_eq!(state.selected, Some(5));
        state.switch_session(1);
        assert_eq!(state.visible_indices(), vec![5]);
        assert!(!state.previous_missing());
        assert!(!state.next_missing());
        assert_eq!(state.stats().unassigned, 1);
        state.selected = None;
        assert_eq!(state.missing_neighbor(1), Some(5));
        assert_eq!(state.missing_neighbor(-1), Some(5));
    }

    #[test]
    fn snapshots_match_visible_scope_and_reject_changed_candidates() {
        let mut state = loaded();
        let single_session = state.write_snapshot(true).unwrap();
        assert_eq!(single_session.items.len(), 1);
        assert_eq!(single_session.items[0].index, 1);
        state.toggle_all();
        assert_eq!(state.write_snapshot(true).unwrap().items.len(), 2);
        assert!(state.validate_snapshot(&single_session).is_ok());
        state.select(1);
        state
            .stage_coordinates(Coordinates::new(-20.0, -70.0).unwrap())
            .unwrap();
        assert!(state.validate_snapshot(&single_session).is_err());
        let staged = state.write_snapshot(false).unwrap();
        state.directory_generation += 1;
        assert!(state.validate_snapshot(&staged).is_err());
    }

    #[test]
    fn greens_are_protected_and_settings_preserve_manual_proposals() {
        let mut state = loaded();
        assert!(
            state
                .stage_coordinates(Coordinates::new(1.0, 2.0).unwrap())
                .is_err()
        );
        assert!(state.manual(Method::CopyNext).is_err());
        state.select(1);
        state
            .stage_coordinates(Coordinates::new(-35.5, -120.25).unwrap())
            .unwrap();
        let manual = state.photos[1].candidate;
        state
            .apply_settings(
                Settings {
                    adjacent_gap_seconds: 1,
                    reference_window_seconds: 1,
                },
                CoordinateFormat::Dms,
            )
            .unwrap();
        assert_eq!(state.photos[1].candidate, manual);
        assert_eq!(state.coordinate_format, CoordinateFormat::Dms);
        state.restore_auto().unwrap();
        assert!(state.photos[1].candidate.is_none());
        assert!(!state.has_manual_candidates());
    }

    #[test]
    fn old_directory_and_preview_results_cannot_replace_new_state() {
        let mut state = loaded();
        let old_generation = state.preview_generation;
        state.request_preview(100, 80);
        let after_size = state.preview_generation;
        assert!(after_size > old_generation);
        let old_cancel = state.preview_cancel.clone();
        state.select(1);
        assert!(old_cancel.load(Ordering::Acquire));
        assert!(!state.accept(Response::Preview(
            after_size,
            Ok(DynamicImage::new_rgb8(100, 80))
        )));
        assert!(state.preview.is_none());
        assert!(!state.accept(Response::Scan(
            99,
            Ok(ScanResult {
                photos: Vec::new(),
                directories: Vec::new(),
                warnings: Vec::new()
            })
        )));
        assert_eq!(state.photos.len(), 4);
    }

    #[test]
    fn repeated_frames_do_not_cancel_or_duplicate_the_same_preview() {
        let mut state = loaded();
        let (requests, pending) = mpsc::channel();
        state.workers.requests = Some(requests);
        state.request_preview(100, 80);
        let generation = state.preview_generation;
        let cancel = state.preview_cancel.clone();
        for _ in 0..100 {
            state.poll();
            state.request_preview(100, 80);
        }
        assert_eq!(state.preview_generation, generation);
        assert!(!cancel.load(Ordering::Acquire));
        assert!(matches!(pending.try_recv(), Ok(Request::Preview { .. })));
        assert!(pending.try_recv().is_err());
        state.accept(Response::Preview(
            generation,
            Ok(DynamicImage::new_rgb8(100, 80)),
        ));
        let revision = state.preview_revision;
        state.poll();
        state.request_preview(100, 80);
        assert_eq!(state.preview_revision, revision);
        assert!(state.preview.is_some());
        assert!(pending.try_recv().is_err());
    }

    #[test]
    fn write_completion_refreshes_the_same_photo_with_a_live_cancel_token() {
        let mut state = loaded();
        let (requests, pending) = mpsc::channel();
        state.workers.requests = Some(requests);
        state.select(1);
        state.request_preview(100, 80);
        let generation = state.preview_generation;
        let canceled = state.preview_cancel.clone();
        let snapshot = state.write_snapshot(false).unwrap();
        state.confirm_write(snapshot).unwrap();
        assert!(canceled.load(Ordering::Acquire));
        state.accept(Response::WriteFinished(state.directory_generation, 1, 1));
        assert!(!state.preview_cancel.load(Ordering::Acquire));
        assert!(state.preview_generation > generation);
        state.schedule_preview();
        let requests: Vec<_> = pending.try_iter().collect();
        assert_eq!(requests.len(), 3);
        assert!(matches!(requests[0], Request::Preview { .. }));
        assert!(matches!(requests[1], Request::Write { .. }));
        match &requests[2] {
            Request::Preview {
                generation,
                cancel,
                path,
                ..
            } => {
                assert_eq!(*generation, state.preview_generation);
                assert!(!cancel.load(Ordering::Acquire));
                assert_eq!(path, &state.photos[1].path);
            }
            _ => panic!("write completion must request a fresh preview"),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FileSnapshot {
        accessed: SystemTime,
        modified: SystemTime,
        created: Option<SystemTime>,
        #[cfg(unix)]
        identity: (u64, u64),
    }

    impl FileSnapshot {
        fn capture(path: &Path) -> Self {
            let metadata = fs::metadata(path).unwrap();
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            Self {
                accessed: metadata.accessed().unwrap(),
                modified: metadata.modified().unwrap(),
                created: metadata.created().ok(),
                #[cfg(unix)]
                identity: (metadata.dev(), metadata.ino()),
            }
        }
    }

    /// A tiny test JPEG with one EXIF DateTimeOriginal ASCII value. Keeping
    /// fixture construction local avoids reading any user or downloaded photo.
    fn fixture_jpeg(path: &Path, capture_time: &str) {
        assert_eq!(capture_time.len(), 19);
        let image = image::RgbImage::from_fn(24, 16, |x, y| {
            image::Rgb([(x * 7) as u8, (y * 11) as u8, 120])
        });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&DynamicImage::ImageRgb8(image))
            .unwrap();
        // TIFF header, IFD0's ExifIFD pointer, then DateTimeOriginal in ExifIFD.
        let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0".to_vec();
        exif.extend_from_slice(&1_u16.to_le_bytes());
        exif.extend_from_slice(&0x8769_u16.to_le_bytes());
        exif.extend_from_slice(&4_u16.to_le_bytes());
        exif.extend_from_slice(&1_u32.to_le_bytes());
        exif.extend_from_slice(&26_u32.to_le_bytes());
        exif.extend_from_slice(&0_u32.to_le_bytes());
        exif.extend_from_slice(&1_u16.to_le_bytes());
        exif.extend_from_slice(&0x9003_u16.to_le_bytes());
        exif.extend_from_slice(&2_u16.to_le_bytes());
        exif.extend_from_slice(&20_u32.to_le_bytes());
        exif.extend_from_slice(&44_u32.to_le_bytes());
        exif.extend_from_slice(&0_u32.to_le_bytes());
        exif.extend_from_slice(capture_time.as_bytes());
        exif.push(0);
        let mut with_exif = Vec::from(&jpeg[..2]);
        with_exif.extend_from_slice(&[0xff, 0xe1]);
        with_exif.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        with_exif.extend_from_slice(&exif);
        with_exif.extend_from_slice(&jpeg[2..]);
        fs::write(path, with_exif).unwrap();
    }

    fn accept_next_background_result(state: &mut GuiState) {
        let response = state
            .workers
            .responses
            .recv_timeout(StdDuration::from_secs(60))
            .expect("background photo operation did not complete");
        state.accept(response);
    }

    #[test]
    fn actual_background_scan_preview_and_confirmed_write_finish_before_shutdown_returns() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("照片 with spaces");
        fs::create_dir(&directory).unwrap();
        let anchor = directory.join("01 参考.jpg");
        let target = directory.join("02 待补.jpg");
        fixture_jpeg(&anchor, "2026:11:23 10:00:00");
        fixture_jpeg(&target, "2026:11:23 10:00:30");
        let original_target = fs::read(&target).unwrap();
        let tool = MetadataTool::discover().expect("built-in metadata engine must be available");
        let anchor_candidate = Candidate {
            coordinates: Coordinates::new(35.5, -120.25).unwrap(),
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
        };
        tool.write_gps(&anchor, &anchor_candidate).unwrap();
        let accessed = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_600_000_001);
        let modified = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_600_000_101);
        for path in [&anchor, &target] {
            // A writable handle works on all three supported desktop systems.
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(
                    FileTimes::new()
                        .set_accessed(accessed)
                        .set_modified(modified),
                )
                .unwrap();
        }
        let anchor_before = FileSnapshot::capture(&anchor);
        let target_before = FileSnapshot::capture(&target);
        let mut state = GuiState::new(directory.clone(), tool.clone());
        while state.loading {
            accept_next_background_result(&mut state);
        }
        assert_eq!(state.photos.len(), 2, "{}", state.status);
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.photos[1].path, target);
        let automatic = state.photos[1].candidate.unwrap();
        assert_eq!(automatic.coordinates, anchor_candidate.coordinates);
        assert_eq!(automatic.provenance.mode, Mode::Auto);
        assert_eq!(automatic.provenance.method, Method::CopyPrevious);
        assert_eq!(FileSnapshot::capture(&anchor), anchor_before);
        assert_eq!(FileSnapshot::capture(&target), target_before);

        state.select(1);
        state.request_preview(96, 64);
        while state.preview.is_none() {
            accept_next_background_result(&mut state);
            assert!(state.preview_status.is_empty(), "{}", state.preview_status);
        }
        assert_eq!(FileSnapshot::capture(&target), target_before);
        state.manual(Method::CopyPrevious).unwrap();
        let manual = state.photos[1].candidate.unwrap();
        assert_eq!(manual.coordinates, anchor_candidate.coordinates);
        assert_eq!(manual.provenance.mode, Mode::Manual);
        assert_eq!(manual.provenance.method, Method::CopyPrevious);

        // Race an outstanding resize preview against a confirmed write, then
        // immediately close. The serialized worker cancels unstarted reads,
        // finishes this approved batch, and restores times before join returns.
        state.request_preview(120, 80);
        state
            .confirm_write(state.write_snapshot(false).unwrap())
            .unwrap();
        state.shutdown();
        state.poll();
        assert!(!state.writing);
        assert_eq!(state.write_progress, (1, 1));
        assert_eq!(state.photos[1].gps, Some(manual.coordinates));
        assert_eq!(state.photos[1].provenance, Some(manual.provenance));
        assert!(state.photos[1].candidate.is_none());
        assert!(state.photos[1].error.is_none(), "{}", state.status);
        assert_eq!(FileSnapshot::capture(&anchor), anchor_before);
        assert_eq!(FileSnapshot::capture(&target), target_before);

        let verified = tool.scan_directory(&directory).unwrap();
        let written = verified.photos.iter().find(|p| p.path == target).unwrap();
        assert_eq!(written.gps, Some(manual.coordinates));
        assert_eq!(written.provenance, Some(manual.provenance));
        assert_eq!(written.captured_at, state.photos[1].captured_at);
        assert_eq!(FileSnapshot::capture(&anchor), anchor_before);
        assert_eq!(FileSnapshot::capture(&target), target_before);
        assert_eq!(
            fs::read(directory.join("02 待补.jpg.photopinfix-original")).unwrap(),
            original_target
        );
    }

    #[test]
    fn shutdown_joins_active_cleanup_and_is_idempotent() {
        let mut state = state();
        let (release, waiting) = mpsc::channel();
        let completed = Arc::new(AtomicBool::new(false));
        let flag = completed.clone();
        state.workers.thread = Some(thread::spawn(move || {
            waiting.recv().unwrap();
            flag.store(true, Ordering::Release);
        }));
        release.send(()).unwrap();
        state.shutdown();
        assert!(completed.load(Ordering::Acquire));
        assert!(state.workers.stopping.load(Ordering::Acquire));
        assert!(state.workers.requests.is_none());
        state.shutdown();
    }
}
