//! Native desktop frontend. Photo I/O and inference stay in the shared engine.
use std::{error::Error, path::PathBuf, time::Duration};

use eframe::egui::{self, Color32, Key, RichText, Sense, TextureHandle, Vec2};
use photopinfix::{
    coordinates::{CoordinateFormat, CoordinateNotation, format_coordinate, parse_coordinate},
    metadata::MetadataTool,
    model::{Coordinates, Method, Photo, PhotoStatus, Settings},
};

use crate::{
    gui_directory::NativeDirectoryDialog,
    gui_map::GuiMap,
    gui_state::{GuiState, WriteSnapshot},
    gui_theme::{META, MUTED, TEXT, TITLE},
};

const GREEN: Color32 = Color32::from_rgb(30, 116, 91);
const YELLOW: Color32 = Color32::from_rgb(162, 106, 27);
const RED: Color32 = Color32::from_rgb(187, 77, 67);
const PURPLE: Color32 = Color32::from_rgb(119, 85, 163);
const BLUE: Color32 = Color32::from_rgb(49, 117, 150);
const INK: Color32 = Color32::from_rgb(36, 43, 47);
const CANVAS: Color32 = Color32::from_rgb(244, 245, 247);
const LINE: Color32 = Color32::from_rgb(224, 228, 231);
const TINT: Color32 = Color32::from_rgb(230, 241, 233);

fn primary(label: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(label)
        .fill(GREEN)
        .stroke(egui::Stroke::NONE)
        .min_size(Vec2::new(100.0, 34.0))
        .corner_radius(6)
}

fn badge(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.09))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(META));
        });
}

fn card(ui: &mut egui::Ui, rect: egui::Rect, content: impl FnOnce(&mut egui::Ui)) {
    ui.painter().rect_filled(rect, 6.0, Color32::WHITE);
    ui.painter().rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(12.0);
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        ui.shrink_clip_rect(inner);
        content(ui);
    });
}

pub fn run(directory: Option<PathBuf>, tool: MetadataTool) -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err("GUI 需要桌面图形会话（X11 / Wayland）。请在桌面终端运行 --gui；当前 SSH 会话可不加 --gui 使用 TUI。".into());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PhotoPinFix")
            .with_inner_size([1440.0, 960.0])
            .with_min_inner_size([880.0, 640.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "PhotoPinFix",
        options,
        Box::new(move |cc| {
            configure(&cc.egui_ctx);
            let desktop = Desktop::new(&cc.egui_ctx, directory, tool);
            crate::gui_launch::ready()?;
            Ok(Box::new(desktop))
        }),
    )
    .map_err(|error| format!("无法打开桌面窗口：{error}"))?;
    Ok(())
}

fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sc".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/NotoSansSC-Regular.otf"))
            .into(),
    );
    // Use one regular face for Latin and Chinese. The bundled symbol fallback
    // also covers navigation glyphs that Ubuntu Light and Noto SC do not have.
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .splice(0..0, ["noto-sc".into(), "Hack".into()]);
    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .unwrap()
        .push("noto-sc".into());
    fonts
}

fn configure(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.set_theme(egui::ThemePreference::Light);
    let mut style = (*ctx.style_of(egui::Theme::Light)).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = CANVAS;
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.extreme_bg_color = Color32::WHITE;
    style.visuals.faint_bg_color = CANVAS;
    style.visuals.text_edit_bg_color = Some(Color32::WHITE);
    style.visuals.selection.bg_fill = TINT;
    style.visuals.selection.stroke = egui::Stroke::new(1.0, GREEN);
    style.visuals.window_corner_radius = egui::CornerRadius::same(16);
    style.visuals.window_stroke = egui::Stroke::new(1.0, LINE);
    style.visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 12],
        blur: 40,
        spread: 2,
        color: Color32::from_black_alpha(28),
    };
    for visual in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.noninteractive,
    ] {
        visual.corner_radius = egui::CornerRadius::same(7);
        visual.bg_stroke = egui::Stroke::new(1.0, LINE);
        visual.fg_stroke = egui::Stroke::new(1.0, INK);
    }
    style.visuals.widgets.inactive.weak_bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.bg_fill = CANVAS;
    style.visuals.widgets.hovered.weak_bg_fill = TINT;
    style.visuals.widgets.hovered.bg_fill = TINT;
    style.visuals.widgets.active.weak_bg_fill = TINT;
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.interact_size.y = 34.0;
    for (kind, size) in [
        (egui::TextStyle::Heading, TITLE),
        (egui::TextStyle::Body, TEXT),
        (egui::TextStyle::Button, TEXT),
        (egui::TextStyle::Small, META),
    ] {
        style
            .text_styles
            .insert(kind, egui::FontId::proportional(size));
    }
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(TEXT));
    ctx.set_style_of(egui::Theme::Light, style);
}

struct Editor {
    latitude: String,
    longitude: String,
    format: CoordinateFormat,
    notation: CoordinateNotation,
    initial_focus: bool,
}

impl Editor {
    fn new(coordinates: Option<Coordinates>, format: CoordinateFormat) -> Self {
        let mut editor = Self {
            latitude: String::new(),
            longitude: String::new(),
            format,
            notation: CoordinateNotation::Signed,
            initial_focus: true,
        };
        if let Some(coordinates) = coordinates {
            editor.set(coordinates);
        }
        editor
    }
    fn coordinates(&self) -> Result<Coordinates, String> {
        Coordinates::new(
            parse_coordinate(&self.latitude, true)?,
            parse_coordinate(&self.longitude, false)?,
        )
    }
    fn set(&mut self, coordinates: Coordinates) {
        self.latitude = format_coordinate(coordinates.lat, true, self.format, self.notation);
        self.longitude = format_coordinate(coordinates.lon, false, self.format, self.notation);
    }
    fn convert(&mut self, format: CoordinateFormat, notation: CoordinateNotation) {
        let coordinates = self.coordinates();
        self.format = format;
        self.notation = notation;
        if let Ok(coordinates) = coordinates {
            self.set(coordinates);
        }
    }
}

enum Destination {
    Directory(PathBuf),
    Rescan,
    Close,
}
enum Dialog {
    Edit(Editor),
    Settings {
        gap: String,
        window: String,
        format: CoordinateFormat,
    },
    Write(WriteSnapshot),
    Leave(Destination),
    Details,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompactView {
    Photo,
    Map,
    Location,
}

struct Desktop {
    state: GuiState,
    map: GuiMap,
    photo: Option<TextureHandle>,
    photo_revision: u64,
    map_scope: String,
    path: String,
    focus_path: bool,
    dialog: Option<Dialog>,
    directory_dialog: Option<NativeDirectoryDialog>,
    dialog_fresh: bool,
    close_allowed: bool,
    scroll_selected: bool,
    compact_view: CompactView,
    welcome: bool,
}

impl Desktop {
    fn new(ctx: &egui::Context, directory: Option<PathBuf>, tool: MetadataTool) -> Self {
        let welcome = directory.is_none();
        let browse_start = directory.clone().unwrap_or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
        });
        Self {
            path: directory
                .as_ref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            state: if welcome {
                GuiState::idle(browse_start, tool)
            } else {
                GuiState::new(browse_start, tool)
            },
            map: GuiMap::new(ctx),
            photo: None,
            photo_revision: u64::MAX,
            map_scope: String::new(),
            focus_path: false,
            dialog: None,
            directory_dialog: None,
            dialog_fresh: false,
            close_allowed: false,
            scroll_selected: true,
            compact_view: CompactView::Photo,
            welcome,
        }
    }
    fn choose_directory(&mut self) {
        if !self.state.writing && self.directory_dialog.is_none() {
            self.directory_dialog = Some(NativeDirectoryDialog::request(Some(
                self.state.directory.clone(),
            )));
        }
    }
    fn open_dialog(&mut self, dialog: Dialog) {
        self.dialog = Some(dialog);
        self.dialog_fresh = true;
    }
    fn result(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.state.status = error;
        }
    }
    fn edit(&mut self) {
        if let Some(photo) = self.state.selected_photo() {
            if photo.gps.is_some() {
                self.state.status = "已有 GPS，保留原位置。".into();
                return;
            }
            self.open_dialog(Dialog::Edit(Editor::new(
                photo.effective_coordinates(),
                self.state.coordinate_format,
            )));
        }
    }
    fn settings(&mut self) {
        self.open_dialog(Dialog::Settings {
            gap: self.state.settings.adjacent_gap_seconds.to_string(),
            window: self.state.settings.reference_window_seconds.to_string(),
            format: self.state.coordinate_format,
        });
    }
    fn write(&mut self, all: bool) {
        match self.state.write_snapshot(all) {
            Ok(snapshot) => self.open_dialog(Dialog::Write(snapshot)),
            Err(error) => self.state.status = error,
        }
    }
    fn navigate(&mut self, destination: Destination, ctx: &egui::Context) {
        if self.state.writing {
            self.state.status = "正在写入，请等待完成。".into();
            return;
        }
        if self.state.has_manual_candidates() {
            self.open_dialog(Dialog::Leave(destination));
        } else {
            self.navigate_now(destination, ctx);
        }
    }
    fn navigate_now(&mut self, destination: Destination, ctx: &egui::Context) {
        match destination {
            Destination::Directory(path) => {
                let result = self.state.open_directory(path);
                if result.is_ok() {
                    self.welcome = false;
                    self.map_scope.clear();
                }
                self.result(result);
            }
            Destination::Rescan => {
                let result = self.state.rescan();
                if result.is_ok() {
                    self.map_scope.clear();
                }
                self.result(result);
            }
            Destination::Close => {
                self.close_allowed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.path = self.state.directory.to_string_lossy().into_owned();
        self.scroll_selected = true;
    }
    fn manual(&mut self, method: Method) {
        let result = self.state.manual(method);
        self.result(result);
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || self.directory_dialog.is_some() {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, Key::L)) {
            self.focus_path = true;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, Key::Q)) {
            self.navigate(Destination::Close, ctx);
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, Key::O)) {
            self.choose_directory();
            return;
        }
        if ctx.text_edit_focused() || self.state.writing || self.state.loading {
            return;
        }
        if self.welcome {
            return;
        }
        let key = |key| ctx.input(|input| input.key_pressed(key));
        if key(Key::ArrowDown) || key(Key::J) {
            self.state.move_selection(1);
            self.scroll_selected = true;
        }
        if key(Key::ArrowUp) || key(Key::K) {
            self.state.move_selection(-1);
            self.scroll_selected = true;
        }
        if key(Key::ArrowLeft) {
            self.state.switch_session(-1);
            self.scroll_selected = true;
        }
        if key(Key::ArrowRight) {
            self.state.switch_session(1);
            self.scroll_selected = true;
        }
        if key(Key::A) {
            self.state.toggle_all();
            self.scroll_selected = true;
        }
        if key(Key::OpenBracket) {
            self.jump_missing(-1);
        }
        if key(Key::CloseBracket) {
            self.jump_missing(1);
        }
        if key(Key::E) {
            self.edit();
        }
        if key(Key::S) {
            self.settings();
        }
        for (key_code, method) in [
            (Key::P, Method::CopyPrevious),
            (Key::N, Method::CopyNext),
            (Key::C, Method::CopyNearest),
            (Key::I, Method::TimeInterpolation),
        ] {
            if key(key_code) {
                self.manual(method);
            }
        }
        if key(Key::U) {
            let result = self.state.restore_auto();
            self.result(result);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, Key::Enter))
            || key(Key::W)
        {
            self.write(true);
        } else if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter)) {
            self.write(false);
        }
        if key(Key::R) {
            self.navigate(Destination::Rescan, ctx);
        }
        if key(Key::Backspace)
            && let Some(parent) = self.state.directory.parent()
        {
            self.navigate(Destination::Directory(parent.to_owned()), ctx);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (logo, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(logo, 7.0, GREEN);
            let center = logo.center() - egui::vec2(0.0, 3.0);
            painter.circle_stroke(center, 6.0, egui::Stroke::new(1.8, Color32::WHITE));
            painter.circle_filled(center, 2.0, Color32::WHITE);
            for side in [-1.0, 1.0] {
                painter.line_segment(
                    [
                        center + egui::vec2(side * 4.0, 4.0),
                        center + egui::vec2(0.0, 11.0),
                    ],
                    egui::Stroke::new(1.8, Color32::WHITE),
                );
            }
            ui.label(RichText::new("PhotoPinFix").size(TEXT).strong());
            ui.add_space(4.0);
            let path_width = (ui.available_width() - 180.0).max(120.0);
            let path = ui
                .add_sized(
                    [path_width, 34.0],
                    egui::TextEdit::singleline(&mut self.path)
                        .id_salt("directory-path")
                        .align(egui::Align2::LEFT_CENTER)
                        .margin(egui::vec2(10.0, 0.0))
                        .font(egui::TextStyle::Body)
                        .hint_text("照片目录 · 输入路径后按 Enter")
                        .desired_width(path_width),
                )
                .on_hover_text("照片目录 · Enter 打开");
            if self.focus_path {
                path.request_focus();
                self.focus_path = false;
            }
            let enter = path.has_focus()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
            if enter {
                path.surrender_focus();
                if self.path.is_empty() {
                    self.choose_directory();
                } else {
                    self.navigate(Destination::Directory(PathBuf::from(&self.path)), ui.ctx());
                }
            }
            if ui
                .add_sized([100.0, 34.0], egui::Button::new("选择文件夹"))
                .on_hover_text("Ctrl+O / Command+O")
                .clicked()
            {
                self.choose_directory();
            }
            ui.menu_button("更多", |ui| {
                if ui.button("打开输入的目录").clicked() {
                    if self.path.is_empty() {
                        self.choose_directory();
                    } else {
                        self.navigate(Destination::Directory(PathBuf::from(&self.path)), ui.ctx());
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(!self.welcome, egui::Button::new("返回上级目录"))
                    .clicked()
                {
                    if let Some(parent) = self.state.directory.parent() {
                        self.navigate(Destination::Directory(parent.to_owned()), ui.ctx());
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(!self.welcome, egui::Button::new("重新扫描 · R"))
                    .clicked()
                {
                    self.navigate(Destination::Rescan, ui.ctx());
                    ui.close();
                }
                ui.separator();
                if ui.button("设置 · S").clicked() {
                    self.settings();
                    ui.close();
                }
                if ui.button("帮助与快捷键 · ?").clicked() {
                    self.open_dialog(Dialog::Details);
                    ui.close();
                }
            });
        });
        ui.add_space(6.0);
    }

    fn progress(&self, ui: &mut egui::Ui) {
        let stats = self.state.stats();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · {} 张照片",
                        self.state.scope_label(),
                        stats.total
                    ))
                    .size(TEXT)
                    .strong(),
                )
                .on_hover_text(self.state.title());
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "原本缺失 {}  ·  已指定 {}  ·  待指定 {}",
                        stats.missing_total(),
                        stats.assigned(),
                        stats.unassigned,
                    ))
                    .size(META)
                    .color(MUTED),
                )
                .on_hover_text("已指定包括自动建议、手动指定和已成功写入；待指定还需要手动定位。");
            });
            ui.horizontal(|ui| {
                for (status, count) in [
                    (PhotoStatus::NativeGps, stats.native_gps),
                    (PhotoStatus::AutoPending, stats.auto_pending),
                    (PhotoStatus::Unassigned, stats.unassigned),
                    (PhotoStatus::ManualPending, stats.manual_pending),
                    (PhotoStatus::Written, stats.written),
                ] {
                    let mut text = egui::text::LayoutJob::default();
                    text.append(
                        &count.to_string(),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(TEXT),
                            color: status_color(status),
                            ..Default::default()
                        },
                    );
                    text.append(
                        &format!("  {}", status.label()),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(META),
                            color: MUTED,
                            ..Default::default()
                        },
                    );
                    ui.label(text).on_hover_text(status.description());
                    ui.add_space(14.0);
                }
            });
        });
        ui.add_space(6.0);
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
        ui.spacing_mut().interact_size.y = 26.0;
        ui.horizontal(|ui| {
            ui.label(RichText::new("照片").size(TEXT).strong());
            ui.label(
                RichText::new(format!("{} 张", self.state.visible_indices().len()))
                    .color(MUTED)
                    .size(META),
            );
        });
        ui.add_enabled_ui(!self.state.loading && !self.state.writing, |ui| {
            ui.horizontal(|ui| {
                let has_sessions = !self.state.sessions.is_empty();
                let previous = has_sessions && self.state.session != Some(0);
                if ui
                    .add_enabled(previous, egui::Button::new("‹"))
                    .on_hover_text("上一次拍摄 · ←")
                    .clicked()
                {
                    self.state.switch_session(-1);
                    self.scroll_selected = true;
                }
                ui.label(RichText::new(self.state.scope_label()).size(META))
                    .on_hover_text(self.state.title());
                let next = has_sessions
                    && self.state.session != Some(self.state.sessions.len().saturating_sub(1));
                if ui
                    .add_enabled(next, egui::Button::new("›"))
                    .on_hover_text("下一次拍摄 · →")
                    .clicked()
                {
                    self.state.switch_session(1);
                    self.scroll_selected = true;
                }
                if ui
                    .add_enabled(
                        has_sessions,
                        egui::Button::selectable(self.state.session.is_none(), "全部"),
                    )
                    .on_hover_text("当前拍摄 / 全部照片 · A")
                    .clicked()
                {
                    self.state.toggle_all();
                    self.scroll_selected = true;
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("photo-list")
            .auto_shrink([false, false])
            .max_height(ui.available_height().max(1.0))
            .show(ui, |ui| {
                for directory in self.state.directories.clone() {
                    let name = directory.file_name().unwrap_or_default().to_string_lossy();
                    if ui
                        .add_enabled(
                            !self.state.loading && !self.state.writing,
                            egui::Button::new(format!("▸  {name}")).truncate(),
                        )
                        .on_hover_text(directory.display().to_string())
                        .clicked()
                    {
                        self.navigate(Destination::Directory(directory), ui.ctx());
                    }
                }
                if self.state.visible_indices().is_empty() && !self.state.loading {
                    ui.label(RichText::new("这里还没有照片\n打开一个目录开始整理。").color(MUTED));
                }
                for index in self.state.visible_indices() {
                    let photo = &self.state.photos[index];
                    let selected = self.state.selected == Some(index);
                    let color = photo_color(photo);
                    let text_width = (ui.available_width() - 20.0).max(1.0);
                    let name = photo.path.file_name().unwrap_or_default().to_string_lossy();
                    let mut name_job = egui::text::LayoutJob::simple(
                        name.to_string(),
                        egui::FontId::proportional(TEXT),
                        INK,
                        text_width,
                    );
                    name_job.wrap.max_rows = 1;
                    name_job.wrap.break_anywhere = true;
                    let name_galley = ui.painter().layout_job(name_job);
                    let status = photo_status(photo);
                    let mut status_job = egui::text::LayoutJob::simple(
                        status.clone(),
                        egui::FontId::proportional(META),
                        color,
                        (text_width - 12.0).max(1.0),
                    );
                    status_job.wrap.max_rows = 1;
                    status_job.wrap.break_anywhere = true;
                    let status_galley = ui.painter().layout_job(status_job);
                    let time = photo.capture_time_label();
                    let single_line_time = ui.painter().layout_no_wrap(
                        time.clone(),
                        egui::FontId::proportional(META),
                        MUTED,
                    );
                    let time_display = if single_line_time.size().x > text_width {
                        time.rsplit_once(' ')
                            .map(|(timestamp, zone)| format!("{timestamp}\n{zone}"))
                            .unwrap_or_else(|| time.clone())
                    } else {
                        time.clone()
                    };
                    let time_galley = ui.painter().layout(
                        time_display,
                        egui::FontId::proportional(META),
                        MUTED,
                        text_width,
                    );
                    // Normal entries are 80 px. Preserve the full timestamp,
                    // including its fractional seconds and timezone, by letting
                    // unusually long timestamps wrap instead of clipping them.
                    let status_y = name_galley.size().y + 2.0;
                    let time_y = status_y + status_galley.size().y + 2.0;
                    let height = (time_y + time_galley.size().y + 12.0).max(80.0);
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), height),
                        Sense::click(),
                    );
                    let painter = ui.painter_at(rect);
                    if selected || response.hovered() {
                        painter.rect_filled(rect, 5.0, if selected { TINT } else { CANVAS });
                    }
                    if selected {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                rect.min + egui::vec2(0.0, 6.0),
                                Vec2::new(2.0, rect.height() - 12.0),
                            ),
                            1.0,
                            GREEN,
                        );
                    }
                    let origin = rect.min + egui::vec2(10.0, 6.0);
                    painter.galley(origin, name_galley, INK);
                    painter.circle_filled(
                        origin + egui::vec2(3.0, status_y + status_galley.size().y / 2.0),
                        2.5,
                        color,
                    );
                    painter.galley(origin + egui::vec2(12.0, status_y), status_galley, color);
                    painter.galley(origin + egui::vec2(0.0, time_y), time_galley, MUTED);
                    if selected && self.scroll_selected {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    let response = response.on_hover_text(format!(
                        "{}\n{}\n{}\n{}",
                        name,
                        status,
                        time,
                        coordinate_label(photo, self.state.coordinate_format)
                    ));
                    if response.clicked() {
                        self.state.select(index);
                    }
                    if response.double_clicked() && !self.state.loading && !self.state.writing {
                        self.state.select(index);
                        self.edit();
                    }
                }
            });
        self.scroll_selected = false;
    }

    fn location_actions(&mut self, ui: &mut egui::Ui) {
        let editable = !self.state.writing
            && !self.state.loading
            && self.state.selected_photo().is_some_and(|p| p.gps.is_none());
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("编辑坐标")
                    .on_hover_text("编辑坐标 · E")
                    .clicked()
                {
                    self.edit();
                }
                ui.menu_button("采用参考位置", |ui| {
                    for (label, method) in [
                        ("前一张已有定位的照片", Method::CopyPrevious),
                        ("后一张已有定位的照片", Method::CopyNext),
                        ("时间最近的定位", Method::CopyNearest),
                        ("按拍摄时间插值", Method::TimeInterpolation),
                    ] {
                        if ui.button(label).clicked() {
                            self.manual(method);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("恢复自动建议").clicked() {
                        let result = self.state.restore_auto();
                        self.result(result);
                        ui.close();
                    }
                });
            });
        });
    }

    fn jump_missing(&mut self, delta: isize) {
        let changed = if delta < 0 {
            self.state.previous_missing()
        } else {
            self.state.next_missing()
        };
        if changed {
            self.scroll_selected = true;
        }
    }

    fn missing_navigation(&mut self, ui: &mut egui::Ui) {
        let mut controls = [
            (-1, "上一张缺失", "[ · 上一张尚未写入 GPS 的照片"),
            (1, "下一张缺失", "] · 下一张尚未写入 GPS 的照片"),
        ];
        // A right-aligned toolbar lays out its children right to left. Keep
        // the visible previous/next order the same as the inspector's order.
        if ui.layout().prefer_right_to_left() {
            controls.reverse();
        }
        ui.horizontal(|ui| {
            for (delta, label, hint) in controls {
                if ui
                    .add_enabled(
                        !self.state.loading
                            && !self.state.writing
                            && self.state.missing_neighbor(delta).is_some(),
                        egui::Button::new(label),
                    )
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.jump_missing(delta);
                }
            }
        });
    }

    fn location_values(&self, ui: &mut egui::Ui) {
        if let Some(photo) = self.state.selected_photo() {
            badge(ui, photo_status(photo), photo_color(photo));
            ui.add_space(4.0);
            for (title, latitude) in [("纬度", true), ("经度", false)] {
                let value = photo.effective_coordinates().map_or_else(
                    || "—".into(),
                    |point| {
                        format_coordinate(
                            if latitude { point.lat } else { point.lon },
                            latitude,
                            self.state.coordinate_format,
                            CoordinateNotation::Hemispheres,
                        )
                    },
                );
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [38.0, 26.0],
                        egui::Label::new(RichText::new(title).color(MUTED).size(META)),
                    );
                    ui.add(egui::Label::new(RichText::new(&value).size(TEXT).strong()).truncate())
                        .on_hover_text(value);
                });
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(if photo.gps.is_some() {
                    "这张照片已有 GPS，保留原位置。"
                } else {
                    "先检查建议，确认后再写入照片。"
                })
                .color(MUTED)
                .size(META),
            );
        } else {
            ui.label(RichText::new("选择一张照片查看位置").color(MUTED));
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui, compact: bool) {
        ui.spacing_mut().item_spacing.y = 6.0;
        if !compact {
            ui.label(RichText::new("照片位置").size(TEXT).strong());
            self.missing_navigation(ui);
        }
        if ui.available_width() >= 460.0 {
            ui.columns(2, |columns| {
                self.location_values(&mut columns[0]);
                self.location_actions(&mut columns[1]);
            });
        } else {
            self.location_values(ui);
            ui.add_space(4.0);
            self.location_actions(ui);
        }
    }

    fn photo(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 6.0;
        if let Some(photo) = self.state.selected_photo() {
            ui.add(
                egui::Label::new(
                    RichText::new(photo.path.file_name().unwrap_or_default().to_string_lossy())
                        .size(TEXT)
                        .strong(),
                )
                .truncate(),
            );
        }
        ui.add_space(6.0);
        let caption_height =
            ui.text_style_height(&egui::TextStyle::Small) + ui.spacing().item_spacing.y + 4.0;
        let available = Vec2::new(
            ui.available_width().max(1.0),
            (ui.available_height() - caption_height).max(1.0),
        );
        self.state.request_preview(
            (available.x * ui.ctx().pixels_per_point()).clamp(64.0, 1800.0) as u32,
            (available.y * ui.ctx().pixels_per_point()).clamp(64.0, 1400.0) as u32,
        );
        if self.photo_revision != self.state.preview_revision {
            self.photo_revision = self.state.preview_revision;
            self.photo = self.state.preview.as_ref().map(|image| {
                let rgba = image.to_rgba8();
                ui.ctx().load_texture(
                    "selected-photo",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                )
            });
        }
        let (rect, _) = ui.allocate_exact_size(available, Sense::hover());
        ui.painter()
            .rect_filled(rect, 8.0, Color32::from_rgb(28, 35, 32));
        if let Some(texture) = &self.photo {
            let size = texture.size_vec2();
            let inset = rect.shrink(8.0);
            let factor = (inset.width() / size.x)
                .min(inset.height() / size.y)
                .max(0.001);
            let target = egui::Rect::from_center_size(rect.center(), size * factor);
            ui.painter().image(
                texture.id(),
                target,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &self.state.preview_status,
                egui::FontId::proportional(TEXT),
                Color32::from_rgb(177, 191, 182),
            );
        }
        ui.add_space(4.0);
        if let Some(photo) = self.state.selected_photo() {
            ui.add(
                egui::Label::new(
                    RichText::new(photo.capture_time_label())
                        .size(META)
                        .color(MUTED),
                )
                .truncate(),
            )
            .on_hover_text(photo.capture_time_label());
        }
    }

    fn welcome_page(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        // Reserve the whole welcome surface, including the area below its
        // centered card, so the containing frame paints the full window.
        ui.allocate_rect(available, Sense::hover());
        let width = available.width().min(720.0);
        let height = available.height().min(420.0);
        let rect = egui::Rect::from_center_size(available.center(), egui::vec2(width, height));
        card(ui, rect, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(32.0);
                let (icon, _) = ui.allocate_exact_size(egui::vec2(64.0, 56.0), Sense::hover());
                let painter = ui.painter_at(icon);
                painter.rect_filled(icon.shrink(2.0), 12.0, TINT);
                let folder = egui::Rect::from_min_size(
                    icon.min + egui::vec2(12.0, 21.0),
                    egui::vec2(40.0, 26.0),
                );
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        icon.min + egui::vec2(12.0, 14.0),
                        egui::vec2(20.0, 13.0),
                    ),
                    4.0,
                    GREEN,
                );
                painter.rect_filled(folder, 4.0, GREEN);
                ui.add_space(16.0);
                ui.label(RichText::new("从一组照片开始").size(TITLE).strong());
                ui.add_space(4.0);
                ui.label(
                    RichText::new("选择照片文件夹，预览并补全缺失的位置。")
                        .color(MUTED)
                        .size(TEXT),
                );
                ui.add_space(20.0);
                if ui
                    .add(
                        primary(RichText::new("选择照片文件夹").color(Color32::WHITE))
                            .min_size(egui::vec2(180.0, 44.0)),
                    )
                    .clicked()
                {
                    self.choose_directory();
                }
                ui.add_space(12.0);
                ui.label(
                    RichText::new("JPEG · PNG · HEIC · 相机 RAW")
                        .color(MUTED)
                        .size(META),
                );
                ui.add_space(24.0);
                ui.label(
                    RichText::new("选择文件夹    →    检查定位建议    →    确认写入")
                        .color(MUTED)
                        .size(META),
                );
                if self.state.status != "选择照片文件夹开始" {
                    ui.add_space(12.0);
                    ui.colored_label(RED, &self.state.status);
                }
            });
        });
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        self.header(ui);
        if self.welcome {
            self.welcome_page(ui);
            return;
        }
        self.progress(ui);
        let wide = ui.available_width() >= 1180.0 && ui.available_height() >= 560.0;
        let footer_height = 40.0;
        let size = Vec2::new(
            ui.available_width(),
            (ui.available_height() - footer_height - ui.spacing().item_spacing.y).max(180.0),
        );
        let (area, _) = ui.allocate_exact_size(size, Sense::hover());
        let gap = 12.0;
        let library_width = if wide { 250.0 } else { 216.0 };
        let library = egui::Rect::from_min_size(area.min, Vec2::new(library_width, area.height()));
        card(ui, library, |ui| self.browser(ui));
        let workspace =
            egui::Rect::from_min_max(egui::pos2(library.right() + gap, area.top()), area.max);
        let scope = format!(
            "{:?}:{:?}:{}",
            self.state.directory,
            self.state.session,
            self.state.photos.len()
        );
        let reset = scope != self.map_scope;
        let points = self.state.map_points();
        if wide {
            self.map_scope = scope;
            let inspector_width = 318.0;
            let preview = egui::Rect::from_min_max(
                workspace.min,
                egui::pos2(
                    workspace.right() - inspector_width - gap,
                    workspace.bottom(),
                ),
            );
            card(ui, preview, |ui| self.photo(ui));
            let side = egui::Rect::from_min_max(
                egui::pos2(preview.right() + gap, workspace.top()),
                workspace.max,
            );
            card(ui, side, |ui| {
                self.inspector(ui, false);
                ui.add_space(4.0);
                ui.separator();
                self.map.show(ui, &points, reset);
            });
        } else {
            card(ui, workspace, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.compact_view, CompactView::Photo, "照片预览");
                    ui.selectable_value(&mut self.compact_view, CompactView::Map, "位置地图");
                    ui.selectable_value(&mut self.compact_view, CompactView::Location, "照片位置");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.missing_navigation(ui);
                    });
                });
                ui.add_space(6.0);
                match self.compact_view {
                    CompactView::Map => {
                        self.map_scope = scope;
                        self.map.show(ui, &points, reset);
                    }
                    CompactView::Photo => self.photo(ui),
                    CompactView::Location => {
                        egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui, true));
                    }
                }
            });
        }
        // Panels occupy fixed rectangles even when the selected tab contains
        // only a few fields. Keep the action bar below the whole workspace.
        ui.advance_cursor_after_rect(area);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), footer_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if !wide {
                    self.location_actions(ui);
                    ui.add_space(4.0);
                }
                if self.state.loading || self.state.writing {
                    ui.spinner();
                }
                let status_width = (ui.available_width() - 252.0).max(24.0);
                ui.add_sized(
                    [status_width, 34.0],
                    egui::Label::new(RichText::new(&self.state.status).size(META).color(MUTED))
                        .truncate(),
                )
                .on_hover_text(&self.state.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_write = !self.state.writing && !self.state.loading;
                    let count = self.state.stats().pending_write();
                    if ui
                        .add_enabled(
                            can_write && count > 0,
                            primary(
                                RichText::new(format!("写入候选 · {count}")).color(Color32::WHITE),
                            ),
                        )
                        .on_hover_text("写入当前范围 · Shift+Enter / W")
                        .clicked()
                    {
                        self.write(true);
                    }
                    let current = self
                        .state
                        .selected_photo()
                        .is_some_and(|p| p.gps.is_none() && p.candidate.is_some());
                    if ui
                        .add_enabled(
                            can_write && current,
                            egui::Button::new("写入当前").min_size(Vec2::new(96.0, 34.0)),
                        )
                        .on_hover_text("写入当前照片 · Enter")
                        .clicked()
                    {
                        self.write(false);
                    }
                });
            },
        );
    }

    fn modal(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let fresh = std::mem::take(&mut self.dialog_fresh);
        let mut dismiss = false;
        let mut accept = false;
        // Consume the key before TextEdit/button focus can interpret it twice.
        let enter =
            !fresh && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
        let escape =
            !fresh && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape));
        egui::Modal::new(egui::Id::unique("desktop-dialog"))
            .frame(
                egui::Frame::window(&ctx.style_of(egui::Theme::Light))
                    .inner_margin(20)
                    .corner_radius(16),
            )
            .show(ctx, |ui| {
                let width = if matches!(&dialog, Dialog::Edit(_)) {
                    460.0
                } else {
                    520.0
                };
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(300.0, width));
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                match &mut dialog {
                    Dialog::Edit(editor) => {
                        ui.label(RichText::new("调整照片位置").size(TITLE).strong());
                        if let Some(photo) = self.state.selected_photo() {
                            let name = photo.path.file_name().unwrap_or_default().to_string_lossy();
                            ui.add(
                                egui::Label::new(RichText::new(name.as_ref()).size(TEXT))
                                    .truncate(),
                            )
                            .on_hover_text(name.as_ref());
                        }
                        ui.label(
                            RichText::new("暂存后可在地图上检查，确认写入才修改照片。")
                                .color(MUTED)
                                .size(META),
                        );
                        ui.add_space(4.0);
                        let mut format = editor.format;
                        let mut notation = editor.notation;
                        egui::Frame::NONE
                            .fill(CANVAS)
                            .corner_radius(8)
                            .inner_margin(6)
                            .show(ui, |ui| {
                                ui.spacing_mut().button_padding = Vec2::new(10.0, 6.0);
                                ui.horizontal(|ui| {
                                    ui.selectable_value(
                                        &mut format,
                                        CoordinateFormat::Decimal,
                                        RichText::new("小数度").size(TEXT),
                                    )
                                    .on_hover_text("F2 切换坐标格式");
                                    ui.selectable_value(
                                        &mut format,
                                        CoordinateFormat::Dms,
                                        RichText::new("度分秒").size(TEXT),
                                    )
                                    .on_hover_text("F2 切换坐标格式");
                                    ui.separator();
                                    ui.selectable_value(
                                        &mut notation,
                                        CoordinateNotation::Signed,
                                        RichText::new("＋ / −").size(TEXT),
                                    )
                                    .on_hover_text("F3 切换方向表示");
                                    ui.selectable_value(
                                        &mut notation,
                                        CoordinateNotation::Hemispheres,
                                        RichText::new("N / S / E / W").size(TEXT),
                                    )
                                    .on_hover_text("F3 切换方向表示");
                                });
                            });
                        if ctx.input(|input| input.key_pressed(Key::F2)) {
                            format = format.toggle();
                        }
                        if ctx.input(|input| input.key_pressed(Key::F3)) {
                            notation = notation.toggle();
                        }
                        if format != editor.format || notation != editor.notation {
                            editor.convert(format, notation);
                        }
                        ui.add_space(8.0);
                        // Keep the existing field IDs when placing them in horizontal rows.
                        let latitude_id = ui.make_persistent_id("gps-latitude");
                        let longitude_id = ui.make_persistent_id("gps-longitude");
                        let latitude = ui
                            .horizontal(|ui| {
                                ui.label(RichText::new("纬度").size(TEXT).strong());
                                ui.add(
                                    egui::TextEdit::singleline(&mut editor.latitude)
                                        .id(latitude_id)
                                        .desired_width(ui.available_width())
                                        .font(egui::FontId::proportional(TEXT))
                                        .margin(egui::vec2(10.0, 10.0))
                                        .hint_text("31.2304 或 31°13′49.44″N"),
                                )
                            })
                            .inner;
                        if editor.initial_focus {
                            latitude.request_focus();
                            editor.initial_focus = false;
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("经度").size(TEXT).strong());
                            ui.add(
                                egui::TextEdit::singleline(&mut editor.longitude)
                                    .id(longitude_id)
                                    .desired_width(ui.available_width())
                                    .font(egui::FontId::proportional(TEXT))
                                    .margin(egui::vec2(10.0, 10.0))
                                    .hint_text("-70.25 或 70°15′00″W"),
                            );
                        });
                        ui.label(
                            RichText::new("纬度 −90° 至 ＋90° · 经度 −180° 至 ＋180°")
                                .color(MUTED)
                                .size(META),
                        );
                        ui.add_space(8.0);
                        let parsed = editor.coordinates();
                        match &parsed {
                            Ok(value) => {
                                ui.label(
                                    RichText::new(format!("✓  {}", value.decimal()))
                                        .color(GREEN)
                                        .size(META),
                                );
                            }
                            Err(error) => {
                                ui.label(RichText::new(error).color(RED).size(META));
                            }
                        }
                        ui.label(
                            RichText::new("Tab 切换输入 · F2 格式 · F3 方向")
                                .color(MUTED)
                                .size(META),
                        );
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            dismiss = ui
                                .button(RichText::new("取消").size(TEXT))
                                .on_hover_text("Esc")
                                .clicked();
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    accept = ui
                                        .add_enabled(
                                            parsed.is_ok(),
                                            primary(
                                                RichText::new("暂存坐标")
                                                    .color(Color32::WHITE)
                                                    .size(TEXT),
                                            ),
                                        )
                                        .on_hover_text("Enter")
                                        .clicked()
                                        || (parsed.is_ok() && enter);
                                },
                            );
                        });
                    }
                    Dialog::Settings {
                        gap,
                        window,
                        format,
                    } => {
                        ui.heading("本次运行设置");
                        ui.label("修改后立即重新计算自动建议，手动候选保留。");
                        ui.separator();
                        ui.label("相邻照片间隔严格小于（秒）");
                        ui.add(egui::TextEdit::singleline(gap).desired_width(180.0));
                        ui.label("每侧参考到目标照片最多相差（秒）");
                        ui.add(egui::TextEdit::singleline(window).desired_width(180.0));
                        ui.horizontal(|ui| {
                            ui.label("经纬度显示");
                            ui.selectable_value(format, CoordinateFormat::Decimal, "小数度");
                            ui.selectable_value(format, CoordinateFormat::Dms, "度分秒");
                        });
                        let parsed = settings_value(gap, window);
                        if let Err(error) = &parsed {
                            ui.colored_label(RED, error);
                        }
                        ui.small("设置只保留在内存中，下次启动恢复默认值。");
                        ui.separator();
                        ui.horizontal(|ui| {
                            dismiss = ui.button("取消  Esc").clicked();
                            accept = parsed.is_ok()
                                && (ui
                                    .add_enabled(
                                        parsed.is_ok(),
                                        primary(
                                            RichText::new("应用设置  Enter").color(Color32::WHITE),
                                        ),
                                    )
                                    .clicked()
                                    || enter);
                        });
                    }
                    Dialog::Write(snapshot) => {
                        ui.heading(format!("确认写入 {} 张照片？", snapshot.items.len()));
                        ui.label(&snapshot.scope);
                        ui.label("写入 GPS 和 PhotoPinFix 来源标记，保留原文件备份与文件时间。");
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .max_height(260.0)
                            .show(ui, |ui| {
                                for item in &snapshot.items {
                                    ui.strong(
                                        item.path.file_name().unwrap_or_default().to_string_lossy(),
                                    );
                                    ui.colored_label(
                                        YELLOW,
                                        format!(
                                            "{} · {} · {}",
                                            item.candidate.coordinates.decimal(),
                                            item.candidate.provenance.mode.as_str(),
                                            item.candidate.provenance.method.label()
                                        ),
                                    );
                                }
                            });
                        ui.separator();
                        ui.horizontal(|ui| {
                            let cancel = ui.button("取消  Esc");
                            if fresh {
                                cancel.request_focus();
                            }
                            dismiss = cancel.clicked() || enter;
                            accept = ui
                                .add(primary(RichText::new("确认写入  Y").color(Color32::WHITE)))
                                .clicked()
                                || (!fresh && ctx.input(|input| input.key_pressed(Key::Y)));
                        });
                    }
                    Dialog::Leave(_) => {
                        ui.heading("离开前丢弃手动候选？");
                        ui.label("还有手动编辑的坐标尚未写入照片。");
                        ui.horizontal(|ui| {
                            let cancel = ui.button("继续编辑  Esc");
                            if fresh {
                                cancel.request_focus();
                            }
                            dismiss = cancel.clicked() || enter;
                            accept = ui.button("丢弃并继续  Y").clicked()
                                || (!fresh && ctx.input(|input| input.key_pressed(Key::Y)));
                        });
                    }
                    Dialog::Details => {
                        ui.heading("详情与快捷键");
                        ui.label("↑↓ 选择 · ←→ Session · A 全部 · E 编辑 · S 设置");
                        ui.label("P 前图 · N 后图 · C 最近 · I 时间插值 · U 恢复自动");
                        ui.label("Enter 写当前 · Shift+Enter / W 写范围 · Y 确认 · Esc 取消");
                        ui.label("地图：拖动平移、滚轮缩放；默认无底图。");
                        ui.label("Ctrl+L 目录 · R 重扫 · Ctrl+Q 退出（macOS 使用 Command）");
                        ui.label("Ctrl+O 选择文件夹；GUI 启动后可以关闭终端。");
                        ui.label("[ / ] 上一张 / 下一张缺失；包括已指定但尚未写入的照片。");
                        ui.separator();
                        ui.label(&self.state.status);
                        if let Some(photo) = self.state.selected_photo() {
                            ui.label(photo.path.display().to_string());
                            if let Some(error) = &photo.error {
                                ui.colored_label(RED, error);
                            }
                        }
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .show(ui, |ui| {
                                for warning in &self.state.warnings {
                                    ui.colored_label(YELLOW, warning);
                                }
                            });
                        dismiss = ui.button("关闭  Esc").clicked() || enter;
                    }
                }
            });
        if accept {
            match dialog {
                Dialog::Edit(editor) => {
                    let result = editor
                        .coordinates()
                        .and_then(|value| self.state.stage_coordinates(value));
                    self.result(result);
                }
                Dialog::Settings {
                    gap,
                    window,
                    format,
                } => match settings_value(&gap, &window) {
                    Ok(settings) => {
                        let result = self.state.apply_settings(settings, format);
                        self.result(result);
                    }
                    Err(error) => self.state.status = error,
                },
                Dialog::Write(snapshot) => {
                    let result = self.state.confirm_write(snapshot);
                    self.result(result);
                }
                Dialog::Leave(destination) => self.navigate_now(destination, ctx),
                Dialog::Details => {}
            }
        } else if !dismiss && !escape {
            self.dialog = Some(dialog);
        }
    }
}

impl eframe::App for Desktop {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.poll();
        if let Some(result) = self
            .directory_dialog
            .as_mut()
            .and_then(NativeDirectoryDialog::poll)
        {
            self.directory_dialog = None;
            match result {
                Ok(Some(directory)) => self.navigate(Destination::Directory(directory), ctx),
                Ok(None) => {}
                Err(error) => {
                    self.state.status =
                        format!("无法打开系统文件夹选择窗口：{error}。也可在上方输入目录。")
                }
            }
        }
        // Also service scans and confirmed writes while minimized.
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|input| input.viewport().close_requested()) && !self.close_allowed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.navigate(Destination::Close, &ctx);
        }
        self.keys(&ctx);
        ui.painter().rect_filled(ui.max_rect(), 0.0, CANVAS);
        egui::Frame::NONE
            .fill(CANVAS)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.add_enabled_ui(
                    self.dialog.is_none() && self.directory_dialog.is_none() && !self.state.writing,
                    |ui| self.content(ui),
                );
            });
        self.modal(&ctx);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.state.shutdown();
        self.map.shutdown();
    }
}

fn settings_value(gap: &str, window: &str) -> Result<Settings, String> {
    let settings = Settings {
        adjacent_gap_seconds: gap.trim().parse().map_err(|_| "相邻间隔需要正整数秒")?,
        reference_window_seconds: window.trim().parse().map_err(|_| "参考时差需要正整数秒")?,
    };
    settings.validate()?;
    Ok(settings)
}

fn photo_color(photo: &Photo) -> Color32 {
    status_color(photo.status())
}

fn status_color(status: PhotoStatus) -> Color32 {
    match status {
        PhotoStatus::NativeGps => GREEN,
        PhotoStatus::Written => BLUE,
        PhotoStatus::AutoPending => YELLOW,
        PhotoStatus::Unassigned => RED,
        PhotoStatus::ManualPending => PURPLE,
    }
}

fn photo_status(photo: &Photo) -> String {
    let status = photo.status();
    photo.candidate.filter(|_| status.is_missing()).map_or_else(
        || status.label().into(),
        |candidate| {
            format!(
                "{} · {}",
                status.label(),
                candidate.provenance.method.label()
            )
        },
    )
}

fn coordinate_label(photo: &Photo, format: CoordinateFormat) -> String {
    photo.effective_coordinates().map_or_else(
        || "纬度 —    经度 —".into(),
        |point| {
            format!(
                "{}    {}",
                format_coordinate(point.lat, true, format, CoordinateNotation::Hemispheres),
                format_coordinate(point.lon, false, format, CoordinateNotation::Hemispheres)
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::TEXT;
    #[test]
    fn bundled_fonts_cover_interface_characters_without_system_fonts() {
        let mut fonts =
            eframe::egui::epaint::text::Fonts::new(Default::default(), super::font_definitions());
        let mut view = fonts.with_pixels_per_point(1.0);
        let source = [
            include_str!("gui.rs"),
            include_str!("gui_map.rs"),
            include_str!("coordinates.rs"),
            include_str!("model.rs"),
        ]
        .join("");
        let chars: std::collections::BTreeSet<_> = source
            .chars()
            .filter(|c| !c.is_ascii() && !c.is_whitespace())
            .collect();
        let missing: Vec<_> = chars
            .into_iter()
            .filter(|&c| !view.has_glyph(&eframe::egui::FontId::proportional(TEXT), c))
            .collect();
        assert!(
            missing.is_empty(),
            "Bundled GUI fonts lack interface characters: {missing:?}"
        );
    }
}
