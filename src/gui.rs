//! Native desktop frontend. Photo I/O and inference stay in the shared engine.
use std::{error::Error, path::PathBuf, time::Duration};

use eframe::egui::{self, Color32, Key, RichText, Sense, TextureHandle, Vec2};
use photopinfix::{
    coordinates::{CoordinateFormat, CoordinateNotation, format_coordinate, parse_coordinate},
    metadata::MetadataTool,
    model::{Coordinates, Method, Mode, Photo, Settings},
};

use crate::{
    gui_map::GuiMap,
    gui_state::{GuiState, WriteSnapshot},
};

const GREEN: Color32 = Color32::from_rgb(111, 212, 159);
const YELLOW: Color32 = Color32::from_rgb(242, 199, 103);
const RED: Color32 = Color32::from_rgb(246, 138, 138);
const MUTED: Color32 = Color32::from_rgb(154, 169, 190);

pub fn run(directory: PathBuf, tool: MetadataTool) -> Result<(), Box<dyn Error>> {
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
            Ok(Box::new(Desktop::new(&cc.egui_ctx, directory, tool)))
        }),
    )
    .map_err(|error| format!("无法打开桌面窗口：{error}"))?;
    Ok(())
}

fn configure(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sc".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/NotoSansSC-Regular.otf"))
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("noto-sc".into());
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = Color32::from_rgb(18, 24, 34);
    style.visuals.window_fill = Color32::from_rgb(25, 33, 46);
    style.visuals.selection.bg_fill = Color32::from_rgb(43, 72, 86);
    style.visuals.selection.stroke = egui::Stroke::new(1.0, GREEN);
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
    ctx.set_style_of(egui::Theme::Dark, style);
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

struct Desktop {
    state: GuiState,
    map: GuiMap,
    photo: Option<TextureHandle>,
    photo_revision: u64,
    map_scope: String,
    path: String,
    focus_path: bool,
    dialog: Option<Dialog>,
    dialog_fresh: bool,
    close_allowed: bool,
    scroll_selected: bool,
}

impl Desktop {
    fn new(ctx: &egui::Context, directory: PathBuf, tool: MetadataTool) -> Self {
        Self {
            path: directory.to_string_lossy().into_owned(),
            state: GuiState::new(directory, tool),
            map: GuiMap::new(ctx),
            photo: None,
            photo_revision: u64::MAX,
            map_scope: String::new(),
            focus_path: false,
            dialog: None,
            dialog_fresh: false,
            close_allowed: false,
            scroll_selected: true,
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
                self.result(result);
            }
            Destination::Rescan => {
                let result = self.state.rescan();
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
        if self.dialog.is_some() {
            return;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, Key::L)) {
            self.focus_path = true;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, Key::Q)) {
            self.navigate(Destination::Close, ctx);
            return;
        }
        if ctx.text_edit_focused() || self.state.writing || self.state.loading {
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
            ui.heading(RichText::new("PhotoPinFix").color(GREEN));
            ui.label(RichText::new("照片定位修复").color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("设置  S").clicked() {
                    self.settings();
                }
                if ui.button("详情 / 帮助").clicked() {
                    self.open_dialog(Dialog::Details);
                }
            });
        });
        ui.horizontal(|ui| {
            let path = ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .id_salt("directory-path")
                    .desired_width((ui.available_width() - 185.0).max(150.0)),
            );
            if self.focus_path {
                path.request_focus();
                self.focus_path = false;
            }
            let enter = path.has_focus()
                && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Enter));
            if ui.button("打开目录").clicked() || enter {
                path.surrender_focus();
                self.navigate(Destination::Directory(PathBuf::from(&self.path)), ui.ctx());
            }
            if ui.button("上级").clicked()
                && let Some(parent) = self.state.directory.parent()
            {
                self.navigate(Destination::Directory(parent.to_owned()), ui.ctx());
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.state.loading && !self.state.writing, |ui| {
                if ui.button("◀ 上次").clicked() {
                    self.state.switch_session(-1);
                    self.scroll_selected = true;
                }
                if ui.button("下次 ▶").clicked() {
                    self.state.switch_session(1);
                    self.scroll_selected = true;
                }
                if ui
                    .selectable_label(self.state.session.is_none(), "全部照片  A")
                    .clicked()
                {
                    self.state.toggle_all();
                    self.scroll_selected = true;
                }
                if ui.button("重扫").clicked() {
                    self.navigate(Destination::Rescan, ui.ctx());
                }
            });
            ui.label(self.state.scope_label());
        });
        ui.label(RichText::new(self.state.title()).color(MUTED).size(13.0));
        ui.separator();
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("browser-actions").show(ui, |ui| self.browser_actions(ui));
        ui.horizontal(|ui| {
            ui.strong("文件浏览器");
            ui.colored_label(GREEN, "● 已有 GPS");
            ui.colored_label(YELLOW, "● 待写入");
            ui.colored_label(RED, "● 缺失");
        });
        let height = ui.available_height().max(1.0);
        egui::ScrollArea::vertical()
            .id_salt("photo-list")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for directory in self.state.directories.clone() {
                    let name = directory.file_name().unwrap_or_default().to_string_lossy();
                    if ui.button(format!("文件夹  {name}  →")).clicked() {
                        self.navigate(Destination::Directory(directory), ui.ctx());
                    }
                }
                let indices = self.state.visible_indices();
                if indices.is_empty() && !self.state.loading {
                    ui.label("这个范围没有照片。可以打开其他目录。");
                }
                for index in indices {
                    let photo = &self.state.photos[index];
                    let selected = self.state.selected == Some(index);
                    let color = photo_color(photo);
                    let text = format!(
                        "{}   {}\n{}\n{}",
                        photo_status(photo),
                        photo.path.file_name().unwrap_or_default().to_string_lossy(),
                        photo.capture_time_label(),
                        coordinate_label(photo, self.state.coordinate_format)
                    );
                    let response = ui.add(
                        egui::Button::new(RichText::new(text).color(color).size(14.0))
                            .selected(selected)
                            .min_size(Vec2::new(ui.available_width(), 76.0))
                            .wrap(),
                    );
                    if selected && self.scroll_selected {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    if response.clicked() {
                        self.state.select(index);
                    }
                    if response.double_clicked() {
                        self.state.select(index);
                        self.edit();
                    }
                }
            });
        self.scroll_selected = false;
    }

    fn browser_actions(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.add_enabled_ui(
            !self.state.writing
                && !self.state.loading
                && self.state.selected_photo().is_some_and(|p| p.gps.is_none()),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("编辑坐标  E").clicked() {
                        self.edit();
                    }
                    for (label, method) in [
                        ("前图 P", Method::CopyPrevious),
                        ("后图 N", Method::CopyNext),
                        ("最近 C", Method::CopyNearest),
                        ("时间插值 I", Method::TimeInterpolation),
                    ] {
                        if ui.button(label).clicked() {
                            self.manual(method);
                        }
                    }
                    if ui.button("恢复自动 U").clicked() {
                        let result = self.state.restore_auto();
                        self.result(result);
                    }
                });
            },
        );
        ui.horizontal_wrapped(|ui| {
            let can_write = !self.state.writing && !self.state.loading;
            if ui
                .add_enabled(can_write, egui::Button::new("写入当前  Enter"))
                .clicked()
            {
                self.write(false);
            }
            if ui
                .add_enabled(
                    can_write,
                    egui::Button::new("写入此范围  Shift+Enter / W")
                        .fill(Color32::from_rgb(37, 72, 63)),
                )
                .clicked()
            {
                self.write(true);
            }
        });
        ui.small("已有 GPS 始终保留；所有写入均需确认。");
    }

    fn photo(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("照片预览");
            if let Some(photo) = self.state.selected_photo() {
                ui.label(photo.path.file_name().unwrap_or_default().to_string_lossy());
            }
        });
        let available = ui.available_size().max(Vec2::splat(1.0));
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
            .rect_filled(rect, 6.0, Color32::from_rgb(11, 16, 23));
        if let Some(texture) = &self.photo {
            let size = texture.size_vec2();
            let factor = (rect.width() / size.x).min(rect.height() / size.y);
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
                egui::FontId::proportional(15.0),
                MUTED,
            );
        }
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        self.header(ui);
        let size = Vec2::new(
            ui.available_width(),
            (ui.available_height() - 55.0).max(100.0),
        );
        let (area, _) = ui.allocate_exact_size(size, Sense::hover());
        let gap = 16.0;
        let half = (area.width() - gap) / 2.0;
        let left = egui::Rect::from_min_size(area.min, Vec2::new(half, area.height()));
        let right = egui::Rect::from_min_max(egui::pos2(left.right() + gap, area.top()), area.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
            ui.shrink_clip_rect(left);
            self.browser(ui);
        });
        let photo_rect = egui::Rect::from_min_size(
            right.min,
            Vec2::new(right.width(), (right.height() - gap) / 2.0),
        );
        let map_rect = egui::Rect::from_min_max(
            egui::pos2(right.left(), photo_rect.bottom() + gap),
            right.max,
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(photo_rect), |ui| {
            ui.shrink_clip_rect(photo_rect);
            self.photo(ui)
        });
        let scope = format!(
            "{:?}:{:?}:{}",
            self.state.directory,
            self.state.session,
            self.state.photos.len()
        );
        let reset = scope != self.map_scope;
        self.map_scope = scope;
        let points = self.state.map_points();
        ui.scope_builder(egui::UiBuilder::new().max_rect(map_rect), |ui| {
            ui.shrink_clip_rect(map_rect);
            self.map.show(ui, &points, reset)
        });
        ui.separator();
        ui.horizontal(|ui| {
            if self.state.loading || self.state.writing {
                ui.spinner();
            }
            ui.label(&self.state.status);
        });
        ui.label(
            RichText::new(
                "↑↓ 选择照片   ←→ 切换拍摄   A 全部   E 编辑   S 设置   Ctrl+L 目录   Ctrl+Q 退出",
            )
            .color(MUTED)
            .size(12.0),
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
        egui::Modal::new(egui::Id::unique("desktop-dialog")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 80.0).clamp(300.0, 600.0));
            match &mut dialog {
                Dialog::Edit(editor) => {
                    ui.heading("编辑 GPS 坐标");
                    ui.label(
                        RichText::new("先暂存为手动候选，确认写入后才修改照片。").color(MUTED),
                    );
                    ui.separator();
                    let mut format = editor.format;
                    let mut notation = editor.notation;
                    ui.horizontal(|ui| {
                        ui.label("格式");
                        ui.selectable_value(&mut format, CoordinateFormat::Decimal, "小数度");
                        ui.selectable_value(&mut format, CoordinateFormat::Dms, "度分秒");
                        ui.small("F2");
                    });
                    ui.horizontal(|ui| {
                        ui.label("方向");
                        ui.selectable_value(&mut notation, CoordinateNotation::Signed, "＋ / −");
                        ui.selectable_value(
                            &mut notation,
                            CoordinateNotation::Hemispheres,
                            "N / S / E / W",
                        );
                        ui.small("F3");
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
                    ui.label("纬度  Latitude  ·  −90° 至 ＋90°");
                    let latitude = ui.add(
                        egui::TextEdit::singleline(&mut editor.latitude)
                            .id_salt("gps-latitude")
                            .desired_width(f32::INFINITY)
                            .hint_text("例如 31.2304 或 31°13′49.44″N"),
                    );
                    if editor.initial_focus {
                        latitude.request_focus();
                        editor.initial_focus = false;
                    }
                    ui.label("经度  Longitude  ·  −180° 至 ＋180°");
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.longitude)
                            .id_salt("gps-longitude")
                            .desired_width(f32::INFINITY)
                            .hint_text("例如 -70.25 或 70°15′00″W"),
                    );
                    ui.add_space(8.0);
                    let parsed = editor.coordinates();
                    match &parsed {
                        Ok(value) => {
                            ui.colored_label(GREEN, format!("✓  {}", value.decimal()));
                        }
                        Err(error) => {
                            ui.colored_label(RED, error);
                        }
                    }
                    ui.small("输入同时接受小数、度分秒、正负号和方位字母。Tab 切换输入框。");
                    ui.separator();
                    ui.horizontal(|ui| {
                        dismiss = ui.button("取消  Esc").clicked();
                        accept = parsed.is_ok()
                            && (ui
                                .add_enabled(parsed.is_ok(), egui::Button::new("暂存坐标  Enter"))
                                .clicked()
                                || enter);
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
                                .add_enabled(parsed.is_ok(), egui::Button::new("应用设置  Enter"))
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
                        accept = ui.button("确认写入  Y").clicked()
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
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.add_enabled_ui(self.dialog.is_none() && !self.state.writing, |ui| {
                self.content(ui)
            });
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
    if photo.gps.is_some() {
        GREEN
    } else if photo.candidate.is_some() {
        YELLOW
    } else {
        RED
    }
}

fn photo_status(photo: &Photo) -> String {
    if photo.gps.is_some() {
        "GPS · 已有位置".into()
    } else if let Some(candidate) = photo.candidate {
        format!(
            "{} · {}",
            if candidate.provenance.mode == Mode::Auto {
                "AUTO"
            } else {
                "手动"
            },
            candidate.provenance.method.label()
        )
    } else {
        "缺失 · 待定位".into()
    }
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
