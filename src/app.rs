use crate::{
    cli::Graphics,
    input::TextInput,
    ui,
    workers::{MapRequest, PhotoRequest, Response, Workers},
};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute, terminal,
};
use photopinfix::{
    coordinates::{CoordinateFormat, CoordinateNotation, format_coordinate, parse_coordinate},
    map::{self, MapPoint, MapScale, MapView},
    metadata::{MetadataTool, WriteOutcome},
    model::{
        Candidate, Coordinates, Method, Mode, Photo, PhotoStats, Provenance, Session, Settings,
    },
    strategy,
};
use ratatui::{
    DefaultTerminal,
    layout::{Rect, Size},
    widgets::TableState,
};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
pub enum BrowserRow {
    Parent(PathBuf),
    Directory(PathBuf),
    Photo(usize),
}
pub enum PendingAction {
    Quit,
    Open(PathBuf),
    Rescan,
}
pub enum Modal {
    Help,
    Info(String),
    Settings {
        fields: [TextInput; 2],
        focus: usize,
        format: CoordinateFormat,
        keep_original_backup: bool,
        error: String,
    },
    Edit {
        index: usize,
        fields: [TextInput; 2],
        focus: usize,
        format: CoordinateFormat,
        notation: CoordinateNotation,
        error: String,
    },
    Confirm {
        indices: Vec<usize>,
        confirm: bool,
        keep_original_backup: bool,
    },
    Leave {
        action: PendingAction,
        confirm: bool,
    },
}

pub struct App {
    pub directory: PathBuf,
    pub photos: Vec<Photo>,
    pub directories: Vec<PathBuf>,
    pub sessions: Vec<Session>,
    pub session: Option<usize>,
    pub last_session: usize,
    pub rows: Vec<BrowserRow>,
    pub table: TableState,
    pub settings: Settings,
    pub coordinate_format: CoordinateFormat,
    pub modal: Option<Modal>,
    pub status: String,
    pub issues: Vec<String>,
    pub loading: bool,
    pub writing: bool,
    pub quit: bool,
    pub photo_image: Option<Protocol>,
    pub photo_status: String,
    pub map_image: Option<Protocol>,
    pub map_status: String,
    pub map_scale: Option<MapScale>,
    pub map_loading: bool,
    pub map_tick: usize,
    pub attribution: String,
    pub map_view: MapView,
    pub background: bool,
    pub source: usize,
    pub graphics_name: String,
    pub keyboard_enhanced: bool,
    pub photo_area: Rect,
    pub map_area: Rect,
    pub file_area: Rect,
    pub dirty: bool,
    pub clear: bool,
    tool: MetadataTool,
    workers: Workers,
    directory_generation: u64,
    photo_generation: u64,
    map_generation: u64,
    photo_dirty: bool,
    map_dirty: bool,
    map_cancel: Arc<AtomicBool>,
    map_background: Option<image::DynamicImage>,
    map_background_key: Option<u64>,
    map_scope: u64,
}

struct TerminalModes {
    enhanced: bool,
}
impl Drop for TerminalModes {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableMouseCapture, DisableBracketedPaste);
        if self.enhanced {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
    }
}

pub fn run(
    terminal: &mut DefaultTerminal,
    directory: PathBuf,
    tool: MetadataTool,
    graphics: Graphics,
) -> io::Result<()> {
    let mut picker = if matches!(graphics, Graphics::Halfblocks) {
        Picker::halfblocks()
    } else {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    };
    match graphics {
        Graphics::Sixel => picker.set_protocol_type(ProtocolType::Sixel),
        Graphics::Kitty => picker.set_protocol_type(ProtocolType::Kitty),
        Graphics::Iterm2 => picker.set_protocol_type(ProtocolType::Iterm2),
        _ => {}
    }
    let enhanced = if matches!(graphics, Graphics::Halfblocks) {
        false
    } else {
        terminal::supports_keyboard_enhancement().unwrap_or(false)
    };
    if enhanced {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let _modes = TerminalModes { enhanced };
    let graphics_name = format!("{:?}", picker.protocol_type());
    let workers = Workers::new(tool.clone(), picker);
    let mut app = App {
        directory,
        photos: vec![],
        directories: vec![],
        sessions: vec![],
        session: Some(0),
        last_session: 0,
        rows: vec![],
        table: TableState::default(),
        settings: Settings::default(),
        coordinate_format: CoordinateFormat::default(),
        modal: None,
        status: String::new(),
        issues: vec![],
        loading: false,
        writing: false,
        quit: false,
        photo_image: None,
        photo_status: String::new(),
        map_image: None,
        map_status: String::new(),
        map_scale: None,
        map_loading: false,
        map_tick: 0,
        attribution: String::new(),
        map_view: MapView::fit(&[]),
        background: false,
        source: 0,
        graphics_name,
        keyboard_enhanced: enhanced,
        photo_area: Rect::default(),
        map_area: Rect::default(),
        file_area: Rect::default(),
        dirty: true,
        clear: true,
        tool,
        workers,
        directory_generation: 0,
        photo_generation: 0,
        map_generation: 0,
        photo_dirty: true,
        map_dirty: true,
        map_cancel: Arc::new(AtomicBool::new(false)),
        map_background: None,
        map_background_key: None,
        map_scope: 0,
    };
    app.start_scan();
    while !app.quit {
        app.receive();
        if app.map_loading {
            app.map_tick = app.map_tick.wrapping_add(1);
            app.dirty = true;
        }
        if app.dirty {
            if app.clear {
                terminal.clear()?;
                app.clear = false;
            }
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            app.dirty = false;
        }
        app.schedule_images();
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.key(key),
                Event::Resize(_, _) => {
                    app.dirty = true;
                    app.clear = true;
                    app.photo_dirty = true;
                    app.map_dirty = true;
                }
                Event::Paste(text) => app.paste(&text),
                Event::Mouse(mouse) if app.modal.is_none() => {
                    let over_map = app.map_area.contains((mouse.column, mouse.row).into());
                    match mouse.kind {
                        MouseEventKind::ScrollUp if over_map => {
                            app.map_view.zoom_by(1.0);
                            app.map_dirty = true;
                        }
                        MouseEventKind::ScrollDown if over_map => {
                            app.map_view.zoom_by(-1.0);
                            app.map_dirty = true;
                        }
                        MouseEventKind::ScrollUp => app.move_selection(-1),
                        MouseEventKind::ScrollDown => app.move_selection(1),
                        _ => {}
                    }
                    app.dirty = true;
                }
                _ => {}
            }
        }
    }
    app.map_cancel.store(true, Ordering::Relaxed);
    app.workers.shutdown();
    Ok(())
}

impl App {
    pub fn selected_index(&self) -> Option<usize> {
        match self.table.selected().and_then(|i| self.rows.get(i)) {
            Some(BrowserRow::Photo(index)) => Some(*index),
            _ => None,
        }
    }
    pub fn selected_photo(&self) -> Option<&Photo> {
        self.selected_index().and_then(|i| self.photos.get(i))
    }
    pub fn visible_indices(&self) -> Vec<usize> {
        match self.session {
            Some(i) => self
                .sessions
                .get(i)
                .map(|s| s.indices.clone())
                .unwrap_or_default(),
            None => (0..self.photos.len()).collect(),
        }
    }
    pub fn stats(&self) -> PhotoStats {
        PhotoStats::from_photos(
            self.visible_indices()
                .iter()
                .map(|index| &self.photos[*index]),
        )
    }

    fn move_missing(&mut self, delta: isize) {
        if let Some(row) = missing_row(&self.photos, &self.rows, self.table.selected(), delta) {
            self.table.select(Some(row));
            self.selection_changed();
        } else {
            self.status = if delta < 0 {
                "当前范围前方没有缺少 GPS 的照片"
            } else {
                "当前范围后方没有缺少 GPS 的照片"
            }
            .into();
            self.dirty = true;
        }
    }
    pub fn points(&self) -> Vec<MapPoint> {
        let selected = self.selected_index();
        self.visible_indices()
            .into_iter()
            .filter_map(|i| {
                self.photos[i]
                    .effective_coordinates()
                    .map(|coordinates| MapPoint {
                        coordinates,
                        selected: selected == Some(i),
                        existing: self.photos[i].gps.is_some(),
                    })
            })
            .collect()
    }
    pub fn scope_label(&self) -> String {
        if self.photos.is_empty() {
            return "无照片".into();
        }
        self.session
            .map(|s| format!("Session {}/{}", s + 1, self.sessions.len()))
            .unwrap_or_else(|| "全部照片".into())
    }
    pub fn title(&self) -> String {
        let indices = self.visible_indices();
        let mut times = indices
            .iter()
            .map(|i| &self.photos[*i])
            .filter(|p| p.captured_at.is_some());
        let first = times.next();
        let last = times.next_back().or(first);
        let range = match (first, last) {
            (Some(a), Some(b)) => {
                format!("{} — {}", a.capture_time_label(), b.capture_time_label())
            }
            _ => "拍摄时间未知".into(),
        };
        format!(
            "PhotoPinFix · {} · {} · {} 张",
            self.scope_label(),
            range,
            indices.len()
        )
    }
    pub fn update_areas(&mut self, photo: Rect, map: Rect, file: Rect) {
        if Size::new(self.photo_area.width, self.photo_area.height)
            != Size::new(photo.width, photo.height)
        {
            self.photo_generation += 1;
            self.photo_dirty = true;
            self.photo_image = None;
        }
        if Size::new(self.map_area.width, self.map_area.height) != Size::new(map.width, map.height)
        {
            self.invalidate_map_background();
            self.map_dirty = true;
            self.map_image = None;
            self.map_scale = None;
        }
        self.photo_area = photo;
        self.map_area = map;
        self.file_area = file;
    }
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        if let Some(parent) = self.directory.parent() {
            self.rows.push(BrowserRow::Parent(parent.into()));
        }
        self.rows
            .extend(self.directories.iter().cloned().map(BrowserRow::Directory));
        self.rows
            .extend(self.visible_indices().into_iter().map(BrowserRow::Photo));
        let first = self
            .rows
            .iter()
            .position(|r| matches!(r, BrowserRow::Photo(_)))
            .unwrap_or(0);
        self.table = TableState::default().with_selected(if self.rows.is_empty() {
            None
        } else {
            Some(first)
        });
        self.selection_changed();
        self.fit_map();
    }
    fn fit_map(&mut self) {
        self.map_scope = self.map_scope.wrapping_add(1);
        self.invalidate_map_background();
        self.map_view = MapView::fit(&self.points());
        self.map_dirty = true;
        self.dirty = true;
    }
    fn selection_changed(&mut self) {
        self.photo_generation += 1;
        self.photo_image = None;
        self.photo_dirty = true;
        self.map_dirty = true;
        self.dirty = true;
        self.photo_status = if self.selected_index().is_some() {
            "正在读取图片…".into()
        } else {
            "选择照片以预览".into()
        };
    }
    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.table.selected().unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(self.rows.len() - 1);
        if current != next {
            self.table.select(Some(next));
            self.selection_changed();
        }
    }
    fn start_scan(&mut self) {
        self.directory_generation += 1;
        self.loading = true;
        self.photos.clear();
        self.directories.clear();
        self.sessions.clear();
        self.issues.clear();
        self.rows.clear();
        self.table.select(None);
        self.photo_image = None;
        self.map_image = None;
        self.photo_generation += 1;
        self.invalidate_map_background();
        self.map_dirty = true;
        self.status = format!("正在扫描 {}…", self.directory.display());
        self.dirty = true;
        self.workers.scan(
            self.directory_generation,
            self.tool.clone(),
            self.directory.clone(),
        );
    }
    fn receive(&mut self) {
        while let Ok(response) = self.workers.rx.try_recv() {
            match response {
                Response::Scan(generation, result) if generation == self.directory_generation => {
                    self.loading = false;
                    match result {
                        Ok(result) => {
                            self.photos = result.photos;
                            self.directories = result.directories;
                            self.issues = result.warnings;
                            strategy::sort_photos(&mut self.photos);
                            strategy::recompute_candidates(&mut self.photos, self.settings);
                            self.sessions = strategy::build_sessions(&self.photos);
                            self.session = Some(0);
                            self.last_session = 0;
                            self.rebuild_rows();
                            self.status = format!(
                                "找到 {} 张照片 · {} 次拍摄 · {} 条提示（v 查看）",
                                self.photos.len(),
                                self.sessions.len(),
                                self.issues.len()
                            );
                        }
                        Err(error) => {
                            self.status = error.clone();
                            self.issues.push(error);
                        }
                    }
                }
                Response::Photo(id, result) if id == self.photo_generation => match result {
                    Ok(image) => {
                        self.photo_image = Some(image);
                        self.photo_status.clear();
                    }
                    Err(error) => {
                        self.photo_image = None;
                        self.photo_status = error;
                    }
                },
                Response::Map(id, result) if id == self.map_generation => {
                    self.map_background = Some(result.image);
                    self.map_status = result.status;
                    self.attribution = result.attribution;
                    self.map_loading = false;
                    self.map_dirty = true;
                }
                Response::Written(generation, index, candidate, result)
                    if generation == self.directory_generation =>
                {
                    if let Some(issue) =
                        apply_write_result(&mut self.photos[index], candidate, result)
                    {
                        self.issues.push(issue);
                    }
                    self.map_dirty = true;
                }
                Response::WriteFinished(generation, success, total, warnings)
                    if generation == self.directory_generation =>
                {
                    self.writing = false;
                    self.status = write_summary(success, total, warnings);
                }
                _ => continue,
            }
            self.dirty = true;
        }
    }
    fn schedule_images(&mut self) {
        if self.modal.is_some() || self.loading || self.writing {
            return;
        }
        if self.photo_dirty && self.photo_area.width > 0 && self.photo_area.height > 0 {
            self.photo_dirty = false;
            if let Some(photo) = self.selected_photo() {
                let request = PhotoRequest {
                    id: self.photo_generation,
                    path: photo.path.clone(),
                    size: Size::new(self.photo_area.width, self.photo_area.height),
                };
                self.workers.request_photo(request);
            }
        }
    }

    fn invalidate_map_background(&mut self) {
        self.map_generation = self.map_generation.wrapping_add(1);
        self.map_cancel.store(true, Ordering::Relaxed);
        self.map_background = None;
        self.map_background_key = None;
        self.map_loading = false;
        self.attribution.clear();
    }

    /// Called after layout, before drawing: show current dots immediately even
    /// if a different session's tiles are still downloading on the worker.
    pub fn refresh_map(&mut self) {
        if self.loading
            || self.modal.is_some()
            || self.map_area.width == 0
            || self.map_area.height == 0
        {
            return;
        }
        let points = self.points();
        let size = Size::new(self.map_area.width, self.map_area.height);
        let dimensions = self.workers.map_dimensions(size);
        let key = (self.background && !points.is_empty())
            .then(|| map::background_key(&self.map_view, dimensions, self.source, self.map_scope));
        if key != self.map_background_key {
            self.invalidate_map_background();
            self.map_background_key = key;
            self.map_dirty = true;
            if key.is_some() {
                self.map_cancel = Arc::new(AtomicBool::new(false));
                self.map_loading = true;
                self.map_status = "正在加载底图…".into();
                self.workers.request_map(MapRequest {
                    id: self.map_generation,
                    view: self.map_view.clone(),
                    size,
                    source: self.source,
                    cancel: self.map_cancel.clone(),
                });
            }
        }
        if !self.map_dirty {
            return;
        }
        self.map_dirty = false;
        if key.is_none() {
            self.map_status = "无底图".into();
        }
        let (image, scale) =
            self.workers
                .compose_map(self.map_background.as_ref(), &points, &self.map_view, size);
        match image {
            Ok(image) => self.map_image = Some(image),
            Err(error) => {
                self.map_image = None;
                self.map_status = error;
            }
        }
        self.map_scale = scale;
    }
    fn switch_session(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            return;
        }
        let current = self.session.unwrap_or(self.last_session);
        let next = current
            .saturating_add_signed(delta)
            .min(self.sessions.len() - 1);
        self.session = Some(next);
        self.last_session = next;
        self.rebuild_rows();
    }
    fn manual(&mut self, method: Method) {
        if let Some(index) = self.selected_index() {
            if self.photos[index].gps.is_some() {
                self.status = "已有 GPS，跳过修改".into();
                return;
            }
            match strategy::manual_candidate(&self.photos, index, method) {
                Ok(candidate) => {
                    self.photos[index].candidate = Some(candidate);
                    self.status = format!("手动选择 {}；待确认写入", method.as_str());
                    self.map_dirty = true;
                }
                Err(error) => self.status = error,
            }
        }
    }
    fn open_editor(&mut self) {
        if let Some(index) = self.selected_index() {
            if self.photos[index].gps.is_some() {
                self.status = "已有 GPS，跳过修改".into();
                return;
            }
            let format = self.coordinate_format;
            let notation = if format == CoordinateFormat::Dms {
                CoordinateNotation::Hemispheres
            } else {
                CoordinateNotation::Signed
            };
            let fields = self.photos[index]
                .effective_coordinates()
                .map(|p| {
                    [
                        format_coordinate(p.lat, true, format, notation).into(),
                        format_coordinate(p.lon, false, format, notation).into(),
                    ]
                })
                .unwrap_or_default();
            self.modal = Some(Modal::Edit {
                index,
                fields,
                focus: 0,
                format,
                notation,
                error: String::new(),
            });
            self.clear = true;
        }
    }
    fn confirm_write(&mut self, all: bool) {
        let indices = if all {
            self.visible_indices()
        } else {
            self.selected_index().into_iter().collect()
        };
        let indices: Vec<_> = indices
            .into_iter()
            .filter(|i| self.photos[*i].gps.is_none() && self.photos[*i].candidate.is_some())
            .collect();
        if indices.is_empty() {
            self.status = "没有可写入的 GPS 候选；已有 GPS 的文件会跳过".into();
            return;
        }
        self.modal = Some(Modal::Confirm {
            indices,
            confirm: false,
            keep_original_backup: self.settings.keep_original_backup,
        });
        self.clear = true;
    }
    fn request_action(&mut self, action: PendingAction) {
        let manual = self.photos.iter().any(|p| {
            p.candidate
                .as_ref()
                .is_some_and(|c| c.provenance.mode == Mode::Manual)
        });
        if manual {
            self.modal = Some(Modal::Leave {
                action,
                confirm: false,
            });
            self.clear = true;
        } else {
            self.apply_action(action);
        }
    }
    fn apply_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::Quit => self.quit = true,
            PendingAction::Open(path) => {
                self.directory = path;
                self.start_scan();
            }
            PendingAction::Rescan => self.start_scan(),
        }
    }
    fn open_selected_directory(&mut self) {
        if let Some(BrowserRow::Directory(path) | BrowserRow::Parent(path)) = self
            .table
            .selected()
            .and_then(|i| self.rows.get(i))
            .cloned()
        {
            self.request_action(PendingAction::Open(path));
        }
    }
    pub fn key(&mut self, key: KeyEvent) {
        self.dirty = true;
        if self.writing {
            self.status = "正在写入并校验，请稍候…".into();
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.request_action(PendingAction::Quit);
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.request_action(PendingAction::Quit),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('[') => self.move_missing(-1),
            KeyCode::Char(']') => self.move_missing(1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home => {
                self.table.select(Some(0));
                self.selection_changed();
            }
            KeyCode::End if !self.rows.is_empty() => {
                self.table.select(Some(self.rows.len() - 1));
                self.selection_changed();
            }
            KeyCode::Left => self.switch_session(-1),
            KeyCode::Right => self.switch_session(1),
            KeyCode::Char('a') => {
                self.session = if let Some(i) = self.session {
                    self.last_session = i;
                    None
                } else {
                    Some(self.last_session)
                };
                self.rebuild_rows();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.confirm_write(true)
            }
            KeyCode::Char('W') => self.confirm_write(true),
            KeyCode::Enter => {
                if self.selected_index().is_some() {
                    self.confirm_write(false)
                } else {
                    self.open_selected_directory()
                }
            }
            KeyCode::Char('l') => self.open_selected_directory(),
            KeyCode::Backspace => {
                if let Some(parent) = self.directory.parent() {
                    self.request_action(PendingAction::Open(parent.into()));
                }
            }
            KeyCode::Char('e') => self.open_editor(),
            KeyCode::Char('p') => self.manual(Method::CopyPrevious),
            KeyCode::Char('n') => self.manual(Method::CopyNext),
            KeyCode::Char('c') => self.manual(Method::CopyNearest),
            KeyCode::Char('i') => self.manual(Method::TimeInterpolation),
            KeyCode::Char('u') => {
                if let Some(i) = self.selected_index() {
                    self.photos[i].candidate = None;
                    strategy::recompute_candidates(&mut self.photos, self.settings);
                    self.map_dirty = true;
                    self.status = "已恢复自动建议".into();
                }
            }
            KeyCode::Char('s') => {
                self.modal = Some(Modal::Settings {
                    fields: [
                        self.settings.adjacent_gap_seconds.to_string(),
                        self.settings.reference_window_seconds.to_string(),
                    ]
                    .map(TextInput::from),
                    focus: 0,
                    format: self.coordinate_format,
                    keep_original_backup: self.settings.keep_original_backup,
                    error: String::new(),
                });
                self.clear = true;
            }
            KeyCode::Char('m') => {
                self.background = !self.background;
                self.map_dirty = true;
                self.attribution.clear();
            }
            KeyCode::Char('b') => {
                self.source = (self.source + 1) % map::sources().len().max(1);
                self.map_dirty = true;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.map_view.zoom_by(1.0);
                self.map_dirty = true;
            }
            KeyCode::Char('-') => {
                self.map_view.zoom_by(-1.0);
                self.map_dirty = true;
            }
            KeyCode::Char('f') => self.fit_map(),
            KeyCode::Char('H') => {
                self.map_view.pan(-0.2, 0.0);
                self.map_dirty = true;
            }
            KeyCode::Char('L') => {
                self.map_view.pan(0.2, 0.0);
                self.map_dirty = true;
            }
            KeyCode::Char('K') => {
                self.map_view.pan(0.0, -0.2);
                self.map_dirty = true;
            }
            KeyCode::Char('J') => {
                self.map_view.pan(0.0, 0.2);
                self.map_dirty = true;
            }
            KeyCode::Char('r') => self.request_action(PendingAction::Rescan),
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                self.clear = true;
            }
            KeyCode::Char('v') => {
                self.modal = Some(Modal::Info(if self.issues.is_empty() {
                    "暂无错误或提示".into()
                } else {
                    self.issues.join("\n\n")
                }));
                self.clear = true;
            }
            _ => {}
        }
    }
    fn paste(&mut self, text: &str) {
        match &mut self.modal {
            Some(Modal::Edit { fields, focus, .. })
            | Some(Modal::Settings { fields, focus, .. })
                if *focus < 2 =>
            {
                fields[*focus].insert(text)
            }
            _ => {}
        }
        self.dirty = true;
    }
    fn modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        self.clear = true;
        if key.code == KeyCode::Esc {
            return;
        }
        match modal {
            Modal::Help | Modal::Info(_) => {}
            Modal::Confirm {
                indices,
                mut confirm,
                keep_original_backup,
            } => {
                if matches!(key.code, KeyCode::Tab | KeyCode::Left | KeyCode::Right) {
                    confirm = !confirm;
                }
                if key.code == KeyCode::Char('y') || (key.code == KeyCode::Enter && confirm) {
                    let items = indices
                        .into_iter()
                        .filter_map(|i| {
                            self.photos[i]
                                .candidate
                                .map(|c| (i, self.photos[i].path.clone(), c))
                        })
                        .collect();
                    self.writing = true;
                    self.status = "正在写入并校验文件…".into();
                    self.workers.write(
                        self.directory_generation,
                        self.tool.clone(),
                        items,
                        keep_original_backup,
                    );
                } else if !matches!(key.code, KeyCode::Enter | KeyCode::Char('n')) {
                    self.modal = Some(Modal::Confirm {
                        indices,
                        confirm,
                        keep_original_backup,
                    });
                }
            }
            Modal::Leave {
                action,
                mut confirm,
            } => {
                if matches!(key.code, KeyCode::Tab | KeyCode::Left | KeyCode::Right) {
                    confirm = !confirm;
                }
                if key.code == KeyCode::Char('y') || (key.code == KeyCode::Enter && confirm) {
                    self.apply_action(action);
                } else if !matches!(key.code, KeyCode::Enter | KeyCode::Char('n')) {
                    self.modal = Some(Modal::Leave { action, confirm });
                }
            }
            Modal::Settings {
                mut fields,
                mut focus,
                mut format,
                mut keep_original_backup,
                mut error,
            } => {
                let action = settings_key(key, &mut focus, &mut format, &mut keep_original_backup);
                if action == SettingsAction::Cancel {
                    return;
                }
                if action == SettingsAction::Apply {
                    let values = fields.clone().map(|field| field.value);
                    match parse_settings(&values, keep_original_backup) {
                        Ok(settings) => {
                            self.settings = settings;
                            self.coordinate_format = format;
                            strategy::recompute_candidates(&mut self.photos, self.settings);
                            self.map_dirty = true;
                            self.status = "设置已应用，仅本次运行有效；手动候选已保留".into();
                            return;
                        }
                        Err(message) => error = message,
                    }
                } else if action == SettingsAction::Edit && focus < 2 {
                    fields[focus].key(key);
                    error.clear();
                }
                self.modal = Some(Modal::Settings {
                    fields,
                    focus,
                    format,
                    keep_original_backup,
                    error,
                });
            }
            Modal::Edit {
                index,
                mut fields,
                mut focus,
                mut format,
                mut notation,
                mut error,
            } => {
                if key.code == KeyCode::Enter && focus == 3 {
                    return;
                }
                if key.code == KeyCode::Enter {
                    match editor_coordinates(&fields) {
                        Ok(coordinates) => {
                            self.photos[index].candidate = Some(Candidate {
                                coordinates,
                                provenance: Provenance {
                                    mode: Mode::Manual,
                                    method: Method::CoordinateInput,
                                },
                            });
                            self.map_dirty = true;
                            self.status = "手动坐标已暂存；按 Enter 确认写入".into();
                            return;
                        }
                        Err(message) => error = message,
                    }
                } else if matches!(key.code, KeyCode::F(2) | KeyCode::F(3)) {
                    let next_format = if key.code == KeyCode::F(2) {
                        format.toggle()
                    } else {
                        format
                    };
                    let next_notation = if key.code == KeyCode::F(3) {
                        notation.toggle()
                    } else {
                        notation
                    };
                    match reformat_inputs(&fields, next_format, next_notation) {
                        Ok(converted) => {
                            fields = converted;
                            format = next_format;
                            notation = next_notation;
                            error.clear();
                        }
                        Err(message) => error = message,
                    }
                } else if !move_focus(key, &mut focus, 4) && focus < 2 {
                    fields[focus].key(key);
                    error.clear();
                }
                self.modal = Some(Modal::Edit {
                    index,
                    fields,
                    focus,
                    format,
                    notation,
                    error,
                });
            }
        }
    }
}

fn parse_settings(fields: &[String; 2], keep_original_backup: bool) -> Result<Settings, String> {
    let parse = |text: &str| {
        text.trim()
            .parse::<i64>()
            .map_err(|_| "请输入大于 0 的整数秒数".to_string())
    };
    Settings {
        adjacent_gap_seconds: parse(&fields[0])?,
        reference_window_seconds: parse(&fields[1])?,
        keep_original_backup,
    }
    .validate()
}

#[derive(Debug, PartialEq, Eq)]
enum SettingsAction {
    Edit,
    Stay,
    Apply,
    Cancel,
}

fn settings_key(
    key: KeyEvent,
    focus: &mut usize,
    format: &mut CoordinateFormat,
    keep_original_backup: &mut bool,
) -> SettingsAction {
    let activate = matches!(key.code, KeyCode::Enter | KeyCode::Char(' '));
    if key.code == KeyCode::Esc || (activate && *focus == 5) {
        SettingsAction::Cancel
    } else if key.code == KeyCode::F(3)
        || (*focus == 3 && (activate || matches!(key.code, KeyCode::Left | KeyCode::Right)))
    {
        *keep_original_backup = !*keep_original_backup;
        SettingsAction::Stay
    } else if key.code == KeyCode::F(2)
        || (*focus == 2 && (activate || matches!(key.code, KeyCode::Left | KeyCode::Right)))
    {
        *format = format.toggle();
        SettingsAction::Stay
    } else if key.code == KeyCode::Enter || (activate && *focus == 4) {
        SettingsAction::Apply
    } else if move_focus(key, focus, 6) {
        SettingsAction::Stay
    } else {
        SettingsAction::Edit
    }
}

fn apply_write_result(
    photo: &mut Photo,
    candidate: Candidate,
    result: Result<WriteOutcome, String>,
) -> Option<String> {
    match result {
        Ok(outcome) => {
            photo.gps = Some(candidate.coordinates);
            photo.provenance = Some(candidate.provenance);
            photo.candidate = None;
            photo.error = None;
            outcome
                .warning
                .map(|warning| format!("{}：已写入；警告：{warning}", photo.path.display()))
        }
        Err(error) => {
            let issue = format!("{}: {error}", photo.path.display());
            photo.error = Some(error);
            Some(issue)
        }
    }
}

fn write_summary(success: usize, total: usize, warnings: usize) -> String {
    let warning = if warnings == 0 {
        String::new()
    } else {
        format!("；警告 {warnings}")
    };
    format!(
        "写入完成：成功 {success}/{total}；失败 {}{warning}（v 查看详情）",
        total - success
    )
}

fn move_focus(key: KeyEvent, focus: &mut usize, count: usize) -> bool {
    match key.code {
        KeyCode::BackTab | KeyCode::Up => *focus = (*focus + count - 1) % count,
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
            *focus = (*focus + count - 1) % count
        }
        KeyCode::Tab | KeyCode::Down => *focus = (*focus + 1) % count,
        _ => return false,
    }
    true
}

pub fn editor_coordinates(fields: &[TextInput; 2]) -> Result<Coordinates, String> {
    let lat = parse_coordinate(&fields[0].value, true).map_err(|e| format!("纬度：{e}"))?;
    let lon = parse_coordinate(&fields[1].value, false).map_err(|e| format!("经度：{e}"))?;
    Coordinates::new(lat, lon)
}

fn reformat_inputs(
    fields: &[TextInput; 2],
    format: CoordinateFormat,
    notation: CoordinateNotation,
) -> Result<[TextInput; 2], String> {
    let mut converted = fields.clone();
    for index in 0..2 {
        if fields[index].value.trim().is_empty() {
            continue;
        }
        let value = parse_coordinate(&fields[index].value, index == 0)?;
        converted[index].replace(format_coordinate(value, index == 0, format, notation));
    }
    Ok(converted)
}

fn missing_row(
    photos: &[Photo],
    rows: &[BrowserRow],
    current: Option<usize>,
    delta: isize,
) -> Option<usize> {
    if delta == 0 {
        return None;
    }
    let eligible = |(position, row): &(usize, &BrowserRow)| {
        matches!(row, BrowserRow::Photo(index) if photos[*index].status().is_missing())
            && current.is_none_or(|current| {
                if delta < 0 {
                    *position < current
                } else {
                    *position > current
                }
            })
    };
    if delta < 0 {
        rows.iter()
            .enumerate()
            .rev()
            .find(eligible)
            .map(|(position, _)| position)
    } else {
        rows.iter()
            .enumerate()
            .find(eligible)
            .map(|(position, _)| position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_rows_skip_directories_and_persisted_gps_without_wrapping() {
        let coordinates = Coordinates::new(1.0, 2.0).unwrap();
        let candidate = Candidate {
            coordinates,
            provenance: Provenance {
                mode: Mode::Auto,
                method: Method::CopyPrevious,
            },
        };
        let photo = Photo {
            path: "photo.jpg".into(),
            captured_at: None,
            capture_time_info: Default::default(),
            gps: None,
            provenance: None,
            candidate: None,
            error: None,
        };
        let mut photos = vec![photo; 5];
        photos[0].gps = Some(coordinates);
        photos[1].candidate = Some(candidate);
        photos[3].candidate = Some(Candidate {
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
            ..candidate
        });
        photos[4].gps = Some(coordinates);
        photos[4].provenance = Some(candidate.provenance);
        let rows = [
            BrowserRow::Parent("/".into()),
            BrowserRow::Directory("sub".into()),
            BrowserRow::Photo(0),
            BrowserRow::Photo(1),
            BrowserRow::Photo(2),
            BrowserRow::Photo(3),
            BrowserRow::Photo(4),
        ];
        assert_eq!(missing_row(&photos, &rows, Some(1), 1), Some(3));
        assert_eq!(missing_row(&photos, &rows, Some(3), 1), Some(4));
        assert_eq!(missing_row(&photos, &rows, Some(4), 1), Some(5));
        assert_eq!(missing_row(&photos, &rows, Some(5), 1), None);
        assert_eq!(missing_row(&photos, &rows, Some(3), -1), None);
        assert_eq!(missing_row(&photos, &rows, Some(6), -1), Some(5));
        assert_eq!(missing_row(&photos, &rows[..3], None, 1), None);
        assert_eq!(missing_row(&photos, &rows, None, 1), Some(3));
        assert_eq!(missing_row(&photos, &rows, None, -1), Some(5));
    }

    #[test]
    fn settings_reject_values_that_cannot_be_used_for_time_differences() {
        assert!(parse_settings(&["60".into(), i64::MAX.to_string()], false).is_err());
        assert!(parse_settings(&["0".into(), "600".into()], true).is_err());
        assert_eq!(
            parse_settings(&[" 59 ".into(), "601".into()], true).unwrap(),
            Settings {
                adjacent_gap_seconds: 59,
                reference_window_seconds: 601,
                keep_original_backup: true,
            }
        );
    }

    #[test]
    fn settings_backup_switch_supports_focus_activation_and_draft_cancellation() {
        let saved = Settings::default();
        assert!(!saved.keep_original_backup);
        let mut draft = saved.keep_original_backup;
        let mut focus = 0;
        let mut format = CoordinateFormat::Decimal;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            settings_key(key(KeyCode::F(3)), &mut focus, &mut format, &mut draft),
            SettingsAction::Stay
        );
        assert!(draft);
        assert_eq!(
            settings_key(key(KeyCode::Esc), &mut focus, &mut format, &mut draft),
            SettingsAction::Cancel
        );
        assert!(
            !saved.keep_original_backup,
            "Cancelled draft must not change saved settings"
        );
        assert_eq!(
            settings_key(key(KeyCode::Enter), &mut focus, &mut format, &mut draft),
            SettingsAction::Apply
        );
        assert!(
            parse_settings(&["60".into(), "600".into()], draft)
                .unwrap()
                .keep_original_backup
        );
        for expected in 1..=5 {
            assert_eq!(
                settings_key(key(KeyCode::Tab), &mut focus, &mut format, &mut draft),
                SettingsAction::Stay
            );
            assert_eq!(focus, expected);
        }
        assert_eq!(
            settings_key(key(KeyCode::Char(' ')), &mut focus, &mut format, &mut draft),
            SettingsAction::Cancel
        );
        settings_key(key(KeyCode::BackTab), &mut focus, &mut format, &mut draft);
        assert_eq!(focus, 4);
        assert_eq!(
            settings_key(key(KeyCode::Char(' ')), &mut focus, &mut format, &mut draft),
            SettingsAction::Apply
        );
        settings_key(key(KeyCode::Up), &mut focus, &mut format, &mut draft);
        assert_eq!(focus, 3);
        assert_eq!(
            settings_key(key(KeyCode::Enter), &mut focus, &mut format, &mut draft),
            SettingsAction::Stay
        );
        assert!(!draft);
        assert_eq!(
            settings_key(key(KeyCode::Char(' ')), &mut focus, &mut format, &mut draft),
            SettingsAction::Stay
        );
        assert!(draft);
        settings_key(key(KeyCode::BackTab), &mut focus, &mut format, &mut draft);
        assert_eq!(focus, 2);
        assert_eq!(
            settings_key(key(KeyCode::Enter), &mut focus, &mut format, &mut draft),
            SettingsAction::Stay
        );
        assert_eq!(format, CoordinateFormat::Dms);
    }

    #[test]
    fn successful_write_with_cleanup_warning_is_written_and_warning_stays_visible() {
        let candidate = Candidate {
            coordinates: Coordinates::new(31.2, 121.4).unwrap(),
            provenance: Provenance {
                mode: Mode::Auto,
                method: Method::TimeInterpolation,
            },
        };
        let mut photo = Photo {
            path: "photo.jpg".into(),
            captured_at: None,
            capture_time_info: Default::default(),
            gps: None,
            provenance: None,
            candidate: Some(candidate),
            error: Some("old error".into()),
        };
        let issue = apply_write_result(
            &mut photo,
            candidate,
            Ok(WriteOutcome {
                warning: Some("无法删除备份，已保留".into()),
            }),
        )
        .unwrap();
        assert_eq!(photo.status(), photopinfix::model::PhotoStatus::Written);
        assert_eq!(photo.gps, Some(candidate.coordinates));
        assert!(photo.candidate.is_none() && photo.error.is_none());
        assert!(issue.contains("已写入；警告") && issue.contains("已保留"));
        let summary = write_summary(1, 1, 1);
        assert!(summary.contains("成功 1/1；失败 0；警告 1") && summary.contains("v 查看详情"));
        photo.gps = None;
        photo.provenance = None;
        photo.candidate = Some(candidate);
        assert!(
            apply_write_result(&mut photo, candidate, Err("写入失败".into()))
                .unwrap()
                .contains("写入失败")
        );
        assert!(photo.gps.is_none() && photo.candidate.is_some() && photo.error.is_some());
    }

    #[test]
    fn coordinate_editor_converts_notation_without_changing_location() {
        let inputs = [
            TextInput::from("S33 30 0".to_owned()),
            TextInput::from("-70.25".to_owned()),
        ];
        let expected = editor_coordinates(&inputs).unwrap();
        for format in [CoordinateFormat::Decimal, CoordinateFormat::Dms] {
            for notation in [CoordinateNotation::Signed, CoordinateNotation::Hemispheres] {
                let converted = reformat_inputs(&inputs, format, notation).unwrap();
                assert_eq!(editor_coordinates(&converted).unwrap(), expected);
            }
        }
        assert!(
            editor_coordinates(&[TextInput::from("+33S".to_owned()), inputs[1].clone()]).is_err()
        );
    }
}
