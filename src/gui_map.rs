//! Desktop map panel. Images, tiles and textures remain in memory.
//!
//! There is one pending request and one pending response; a busy map service
//! cannot accumulate a queue of image buffers while the user moves the view.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
};

use crate::gui_theme::{META, MUTED, TEXT};
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use photopinfix::map::{self, MapPoint, MapRenderer, MapScale, MapView};

const SCALE_CELL_POINTS: f32 = 8.0;
const MAP_COLOR: Color32 = Color32::from_rgb(241, 246, 242);
const INK: Color32 = Color32::from_rgb(32, 44, 50);
const TEAL: Color32 = Color32::from_rgb(23, 122, 105);
const AMBER: Color32 = Color32::from_rgb(183, 124, 35);
const GRID: Color32 = Color32::from_rgb(222, 232, 225);

struct RenderRequest {
    generation: u64,
    view: MapView,
    size: [u32; 2],
    source: usize,
    cancel: Arc<AtomicBool>,
}

struct RenderResponse {
    generation: u64,
    image: ColorImage,
    status: String,
    attribution: String,
}

/// Call `show` in the map's allocated panel. Set `reset_view` when changing
/// directory/session/scope; selecting another photo should leave it false.
pub struct GuiMap {
    view: MapView,
    fitted: bool,
    scope: u64,
    background: bool,
    source: usize,
    sources: Vec<String>,
    generation: u64,
    requested_key: Option<u64>,
    request: Arc<Mutex<Option<RenderRequest>>>,
    response: Arc<Mutex<Option<RenderResponse>>>,
    wake: Option<SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
    cancel: Option<Arc<AtomicBool>>,
    texture: Option<TextureHandle>,
    status: String,
    attribution: String,
    loading: bool,
}

impl GuiMap {
    pub fn new(context: &egui::Context) -> Self {
        let request = Arc::new(Mutex::new(None::<RenderRequest>));
        let response = Arc::new(Mutex::new(None::<RenderResponse>));
        let (wake, receiver) = mpsc::sync_channel(1);
        let worker_request = Arc::clone(&request);
        let worker_response = Arc::clone(&response);
        let context = context.clone();
        let worker = thread::spawn(move || {
            let mut renderer = MapRenderer::new();
            while receiver.recv().is_ok() {
                let Some(request) = worker_request
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                else {
                    continue;
                };
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let result = renderer.render_background_cancellable(
                    &request.view,
                    request.size[0],
                    request.size[1],
                    request.source,
                    &request.cancel,
                );
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let rgba = result.image.to_rgba8();
                let image = ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    rgba.as_raw(),
                );
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                *worker_response
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(RenderResponse {
                    generation: request.generation,
                    image,
                    status: result.status,
                    attribution: result.attribution,
                });
                context.request_repaint();
            }
        });
        Self {
            view: MapView::default(),
            fitted: false,
            scope: 0,
            background: false,
            source: 0,
            sources: map::sources(),
            generation: 0,
            requested_key: None,
            request,
            response,
            wake: Some(wake),
            worker: Some(worker),
            cancel: None,
            texture: None,
            status: "无底图".into(),
            attribution: String::new(),
            loading: false,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, points: &[MapPoint], reset_view: bool) {
        if reset_view || !self.fitted {
            self.scope = self.scope.wrapping_add(1);
            self.view = MapView::fit(points);
            self.fitted = true;
        }
        ui.scope(|ui| {
            ui.spacing_mut().button_padding = Vec2::new(7.0, 3.0);
            ui.spacing_mut().item_spacing = Vec2::new(5.0, 6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("位置分布").size(TEXT).strong().color(INK));
                ui.label(
                    RichText::new(points.len().to_string())
                        .size(META)
                        .color(MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if mode_button(ui, "底图", self.background).clicked() {
                        self.background = true;
                    }
                    if mode_button(ui, "点位", !self.background).clicked() {
                        self.background = false;
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui.small_button("−").on_hover_text("缩小").clicked() {
                    self.view.zoom_by(-1.0);
                }
                if ui.small_button("＋").on_hover_text("放大").clicked() {
                    self.view.zoom_by(1.0);
                }
                if ui
                    .small_button("适配")
                    .on_hover_text("显示全部位置")
                    .clicked()
                {
                    self.view = MapView::fit(points);
                }
                if self.background {
                    egui::ComboBox::from_id_salt("photopinfix-map-source")
                        .width(ui.available_width().max(60.0))
                        .selected_text(RichText::new(&self.sources[self.source]).size(META))
                        .show_ui(ui, |ui| {
                            for (index, source) in self.sources.iter().enumerate() {
                                ui.selectable_value(&mut self.source, index, source);
                            }
                        });
                } else {
                    ui.label(RichText::new("无底图 · 参考网格").size(META).color(MUTED));
                }
            });
        });

        let available = ui.available_size();
        // Leave room for one status line. Attribution is painted, wrapping as
        // needed, inside the map and remains visible while a new view loads.
        let status_height = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let size = Vec2::new(available.x.max(1.0), (available.y - status_height).max(1.0));
        let (rect, response) = ui.allocate_exact_size(size, Sense::drag());
        let render_size = image_size(size, ui.ctx().pixels_per_point());
        if response.dragged() {
            pan_pixels(&mut self.view, response.drag_delta(), size, render_size);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if response.hovered() {
            let (scroll, pinch, pointer) = ui.input_mut(|input| {
                let scroll = input.smooth_scroll_delta.y;
                input.smooth_scroll_delta.y = 0.0;
                (scroll, input.zoom_delta(), input.pointer.hover_pos())
            });
            let delta = f64::from(scroll / 160.0)
                + if pinch.is_finite() && pinch > 0.0 {
                    f64::from(pinch).log2()
                } else {
                    0.0
                };
            if delta.abs() > f64::EPSILON {
                let anchor = pointer.unwrap_or_else(|| rect.center()) - rect.min;
                zoom_at(&mut self.view, delta, anchor, size, render_size);
            }
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 7.0, MAP_COLOR);
        if self.background && !points.is_empty() {
            self.submit_if_changed(ui.ctx(), render_size);
            self.receive(ui.ctx());
            if let Some(texture) = &self.texture {
                painter.image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                paint_map_points(&painter, rect, &self.view, points, render_size);
                paint_current_scale(&painter, rect, &self.view, render_size, &self.attribution);
            } else {
                // Locations remain usable while the first tile request loads.
                paint_offline_map(&painter, rect, &self.view, points, render_size);
            }
        } else {
            self.cancel_online_view();
            paint_offline_map(&painter, rect, &self.view, points, render_size);
        }
        if points.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "当前范围暂无 GPS 位置",
                FontId::proportional(META),
                MUTED,
            );
        }
        painter.rect_stroke(rect, 7.0, Stroke::new(1.0, GRID), egui::StrokeKind::Inside);
        response.on_hover_text(
            "滚轮缩放 · 拖动平移 · 绿色为文件已定位，黄色为建议位置 · 大点为当前照片",
        );
        let status = if points.is_empty() {
            "当前范围暂无 GPS 位置".to_owned()
        } else if !self.background {
            "拖动平移 · 滚轮缩放".to_owned()
        } else if self.loading {
            "正在加载底图…".to_owned()
        } else {
            self.status.clone()
        };
        ui.horizontal(|ui| {
            if self.loading {
                ui.add(egui::Spinner::new().size(12.0).color(TEAL));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
            ui.add(egui::Label::new(RichText::new(&status).size(META).color(MUTED)).truncate())
                .on_hover_text(status);
        });
    }

    /// Offline markers are drawn directly, without an image worker or network.
    /// Invalidate any online response before it can replace this new view.
    fn cancel_online_view(&mut self) {
        if self.requested_key.take().is_none() {
            return;
        }
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.generation = self.generation.wrapping_add(1);
        self.request
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.response
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.texture = None;
        self.attribution.clear();
        self.loading = false;
    }

    fn receive(&mut self, context: &egui::Context) {
        let response = self
            .response
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some(response) = response.filter(|item| item.generation == self.generation) else {
            return;
        };
        if let Some(texture) = &mut self.texture {
            texture.set(response.image, TextureOptions::LINEAR);
        } else {
            self.texture = Some(context.load_texture(
                "photopinfix-map",
                response.image,
                TextureOptions::LINEAR,
            ));
        }
        self.status = response.status;
        self.attribution = response.attribution;
        self.loading = false;
    }

    fn submit_if_changed(&mut self, context: &egui::Context, size: [u32; 2]) {
        let key = map::background_key(&self.view, size, self.source, self.scope);
        if self.requested_key == Some(key) || self.wake.is_none() {
            return;
        }
        if let Some(previous) = self.cancel.take() {
            previous.store(true, Ordering::Relaxed);
        }
        self.generation = self.generation.wrapping_add(1);
        self.requested_key = Some(key);
        // A previous view is never stretched under current points while tiles
        // load. Display the current offline grid until this exact view arrives.
        self.texture = None;
        self.attribution.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        *self
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(RenderRequest {
            generation: self.generation,
            view: self.view.clone(),
            size,
            source: self.source,
            cancel,
        });
        self.loading = true;
        if let Some(wake) = &self.wake
            && let Err(mpsc::TrySendError::Disconnected(())) = wake.try_send(())
        {
            self.loading = false;
            self.status = "地图后台已停止".into();
        }
        context.request_repaint();
    }

    /// Cancel pending network work, then join the worker. An in-flight request
    /// uses MapRenderer's bounded HTTP timeout before observing cancellation.
    pub fn shutdown(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.request
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for GuiMap {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn image_size(display: Vec2, pixels_per_point: f32) -> [u32; 2] {
    let ratio = pixels_per_point
        .max(0.1)
        .min((2048.0 / display.x.max(1.0)).min(1536.0 / display.y.max(1.0)));
    [
        (display.x * ratio).round().clamp(1.0, 2048.0) as u32,
        (display.y * ratio).round().clamp(1.0, 1536.0) as u32,
    ]
}

/// Match MapView's aspect-preserving geometry, including its world/zoom caps.
/// MapView::pan uses fitted spans; dragging must instead use actual pixels.
fn units_per_pixel(view: &MapView, size: [u32; 2]) -> f64 {
    let width = f64::from(size[0].max(1));
    let height = f64::from(size[1].max(1));
    ((view.span_x / width).max(view.span_y / height) * 2_f64.powf(-view.zoom))
        .clamp(1.0 / (256.0 * 2_f64.powi(23)), 1.0 / width.max(height))
}

fn pan_pixels(view: &mut MapView, delta: Vec2, display: Vec2, size: [u32; 2]) {
    let units = units_per_pixel(view, size);
    let dx = f64::from(delta.x / display.x.max(1.0)) * f64::from(size[0]) * units;
    let dy = f64::from(delta.y / display.y.max(1.0)) * f64::from(size[1]) * units;
    view.center_x = (view.center_x - dx).rem_euclid(1.0);
    view.center_y = (view.center_y - dy).clamp(0.0, 1.0);
}

fn zoom_at(view: &mut MapView, delta: f64, anchor: Vec2, display: Vec2, size: [u32; 2]) {
    let before = units_per_pixel(view, size);
    view.zoom_by(delta);
    let after = units_per_pixel(view, size);
    let x = f64::from(anchor.x / display.x.max(1.0) - 0.5) * f64::from(size[0]);
    let y = f64::from(anchor.y / display.y.max(1.0) - 0.5) * f64::from(size[1]);
    view.center_x = (view.center_x + x * (before - after)).rem_euclid(1.0);
    view.center_y = (view.center_y + y * (before - after)).clamp(0.0, 1.0);
}

fn mode_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(TEXT).color(if selected {
            TEAL
        } else {
            MUTED
        }))
        .fill(if selected {
            Color32::from_rgb(225, 241, 234)
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(5.0),
    )
}

fn paint_offline_map(
    painter: &egui::Painter,
    rect: Rect,
    view: &MapView,
    points: &[MapPoint],
    size: [u32; 2],
) {
    paint_reference_grid(painter, rect, view, size);
    paint_map_points(painter, rect, view, points, size);
    paint_current_scale(painter, rect, view, size, "");
}

fn paint_map_points(
    painter: &egui::Painter,
    rect: Rect,
    view: &MapView,
    points: &[MapPoint],
    size: [u32; 2],
) {
    // Draw the selected photo last so coincident positions remain visible.
    for selected in [false, true] {
        for point in points.iter().filter(|point| point.selected == selected) {
            let (x, y) = view.pixel_position(point.coordinates, size[0], size[1]);
            let position = rect.min
                + Vec2::new(
                    (x / f64::from(size[0])) as f32 * rect.width(),
                    (y / f64::from(size[1])) as f32 * rect.height(),
                );
            if !rect.expand(16.0).contains(position) {
                continue;
            }
            let color = if point.existing { TEAL } else { AMBER };
            if selected {
                painter.circle_filled(position, 14.0, color.gamma_multiply(0.13));
                painter.circle_filled(position, 8.0, Color32::WHITE);
                painter.circle_filled(position, 5.5, color);
            } else {
                painter.circle_filled(position, 5.0, Color32::WHITE);
                painter.circle_filled(position, 3.25, color);
            }
        }
    }

    if rect.width() > 180.0 && rect.height() > 100.0 {
        let legend = Rect::from_min_size(rect.min + Vec2::splat(10.0), Vec2::new(186.0, 25.0));
        painter.rect_filled(legend, 5.0, Color32::from_white_alpha(230));
        for (x, color, label) in [(10.0, TEAL, "文件已定位"), (88.0, AMBER, "建议位置")] {
            let center = legend.min + Vec2::new(x, 11.5);
            painter.circle_filled(center, 3.0, color);
            painter.text(
                center + Vec2::new(7.0, 0.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(META),
                MUTED,
            );
        }
        painter.text(
            Pos2::new(rect.right() - 17.0, rect.top() + 14.0),
            Align2::CENTER_TOP,
            "N",
            FontId::proportional(META),
            MUTED,
        );
        let tip = Pos2::new(rect.right() - 17.0, rect.top() + 31.0);
        painter.line_segment([tip, tip + Vec2::new(0.0, 9.0)], Stroke::new(1.0, MUTED));
        painter.line_segment([tip, tip + Vec2::new(-3.0, 4.0)], Stroke::new(1.0, MUTED));
        painter.line_segment([tip, tip + Vec2::new(3.0, 4.0)], Stroke::new(1.0, MUTED));
    }
}

fn paint_current_scale(
    painter: &egui::Painter,
    rect: Rect,
    view: &MapView,
    size: [u32; 2],
    attribution: &str,
) {
    let columns = (rect.width() / SCALE_CELL_POINTS)
        .floor()
        .clamp(1.0, f32::from(u16::MAX)) as u16;
    let scale = view.scale(size[0], size[1], columns);
    let fraction = scale
        .as_ref()
        .map_or(0.0, |scale| f32::from(scale.cells) / f32::from(columns));
    paint_overlays(painter, rect, scale.as_ref(), fraction, attribution);
}

/// A reference grid in the same projected coordinates as the markers. Lines
/// stay anchored to the world while panning; they do not suggest roads/routes.
fn paint_reference_grid(painter: &egui::Painter, rect: Rect, view: &MapView, size: [u32; 2]) {
    let units = units_per_pixel(view, size);
    let target = units * f64::from(size[0]) * 64.0 / f64::from(rect.width().max(1.0));
    let magnitude = 10.0_f64.powf(target.log10().floor());
    let multiple = target / magnitude;
    let step = magnitude
        * if multiple <= 1.0 {
            1.0
        } else if multiple <= 2.0 {
            2.0
        } else if multiple <= 5.0 {
            5.0
        } else {
            10.0
        };
    let span_x = f64::from(size[0]) * units;
    let span_y = f64::from(size[1]) * units;
    for (center, span, horizontal) in [
        (view.center_x, span_x, false),
        (view.center_y, span_y, true),
    ] {
        let start = center - span / 2.0;
        let first = (start / step).ceil();
        let count = ((span / step).ceil() as usize).min(64);
        for offset in 0..=count {
            let world = (first + offset as f64) * step;
            let fraction = ((world - start) / span) as f32;
            if !(0.0..=1.0).contains(&fraction) || (horizontal && !(0.0..=1.0).contains(&world)) {
                continue;
            }
            let line = if horizontal {
                let y = rect.top() + fraction * rect.height();
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)]
            } else {
                let x = rect.left() + fraction * rect.width();
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())]
            };
            painter.line_segment(line, Stroke::new(0.75, GRID));
        }
    }
}

fn paint_overlays(
    painter: &egui::Painter,
    rect: Rect,
    scale: Option<&MapScale>,
    scale_fraction: f32,
    attribution: &str,
) {
    let mut bottom = rect.bottom() - 7.0;
    if !attribution.is_empty() {
        let galley = painter.layout(
            attribution.to_owned(),
            FontId::proportional(META),
            INK,
            (rect.width() - 18.0).max(1.0),
        );
        let pos = Pos2::new(rect.left() + 9.0, bottom - galley.size().y);
        painter.rect_filled(
            Rect::from_min_size(pos - Vec2::splat(3.0), galley.size() + Vec2::splat(6.0)),
            4.0,
            Color32::from_white_alpha(238),
        );
        painter.galley(pos, galley, INK);
        bottom = pos.y - 12.0;
    }
    if let Some(scale) = scale {
        let width = rect.width() * scale_fraction;
        let start = Pos2::new(rect.left() + 12.0, bottom - 5.0);
        let label = painter.layout_no_wrap(scale.label.clone(), FontId::proportional(META), INK);
        let bg = Rect::from_min_max(
            Pos2::new(start.x - 5.0, start.y - label.size().y - 12.0),
            Pos2::new(start.x + width.max(label.size().x) + 5.0, start.y + 9.0),
        );
        painter.rect_filled(bg, 5.0, Color32::from_white_alpha(238));
        let stroke = Stroke::new(1.5, TEAL);
        painter.line_segment([start, start + Vec2::new(width, 0.0)], stroke);
        for x in [start.x, start.x + width] {
            painter.line_segment(
                [Pos2::new(x, start.y - 4.0), Pos2::new(x, start.y + 4.0)],
                stroke,
            );
        }
        painter.galley(
            Pos2::new(start.x, start.y - label.size().y - 6.0),
            label,
            INK,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photopinfix::model::Coordinates;

    fn point(lon: f64, lat: f64) -> MapPoint {
        MapPoint {
            coordinates: Coordinates { lat, lon },
            selected: false,
            existing: true,
        }
    }

    #[test]
    fn drag_uses_rendered_aspect_ratio_and_dpi() {
        let points = [point(20.0, 30.0), point(20.1, 60.0)];
        for display in [Vec2::new(800.0, 200.0), Vec2::new(200.0, 800.0)] {
            let size = image_size(display, 2.0);
            let mut view = MapView::fit(&points);
            view.zoom_by(2.0);
            let before = view.pixel_position(points[0].coordinates, size[0], size[1]);
            let delta = Vec2::new(25.0, -18.0);
            pan_pixels(&mut view, delta, display, size);
            let after = view.pixel_position(points[0].coordinates, size[0], size[1]);
            let moved_x = (after.0 - before.0) * f64::from(display.x) / f64::from(size[0]);
            let moved_y = (after.1 - before.1) * f64::from(display.y) / f64::from(size[1]);
            assert!((moved_x - f64::from(delta.x)).abs() < 0.001);
            assert!((moved_y - f64::from(delta.y)).abs() < 0.001);
        }
    }

    #[test]
    fn zoom_keeps_pointer_location_fixed_across_date_line() {
        let points = [point(179.8, 30.0), point(-179.8, 30.1)];
        let display = Vec2::new(600.0, 350.0);
        let size = image_size(display, 2.0);
        let mut view = MapView::fit(&points);
        let before = view.pixel_position(points[0].coordinates, size[0], size[1]);
        let anchor = Vec2::new(
            (before.0 * f64::from(display.x) / f64::from(size[0])) as f32,
            (before.1 * f64::from(display.y) / f64::from(size[1])) as f32,
        );
        zoom_at(&mut view, 1.5, anchor, display, size);
        let after = view.pixel_position(points[0].coordinates, size[0], size[1]);
        assert!((before.0 - after.0).abs() < 0.001);
        assert!((before.1 - after.1).abs() < 0.001);
        assert!((0.0..1.0).contains(&view.center_x));
    }

    #[test]
    fn output_size_preserves_shape_when_limited() {
        assert_eq!(image_size(Vec2::new(800.0, 400.0), 2.0), [1600, 800]);
        assert_eq!(image_size(Vec2::new(4000.0, 2000.0), 2.0), [2048, 1024]);
        assert_eq!(image_size(Vec2::new(400.0, 1600.0), 2.0), [384, 1536]);
    }

    #[test]
    fn delayed_background_cannot_restore_old_session_or_restart_for_marker_changes() {
        let context = egui::Context::default();
        let mut map = GuiMap::new(&context);
        // Replace the network worker with a controlled response source. Nothing
        // is downloaded, and completion can occur after the session changes.
        map.shutdown();
        let (wake, _receiver) = mpsc::sync_channel(1);
        map.wake = Some(wake);
        map.background = true;
        map.scope = 1;
        map.view = MapView::fit(&[point(120.0, 30.0)]);
        map.submit_if_changed(&context, [300, 200]);
        let old_generation = map.generation;
        let old_cancel = map.cancel.as_ref().unwrap().clone();
        let completed = |generation| RenderResponse {
            generation,
            image: ColorImage::from_rgba_unmultiplied([1, 1], &[30, 40, 50, 255]),
            status: "test background".into(),
            attribution: "test source".into(),
        };
        *map.response.lock().unwrap() = Some(completed(old_generation));
        map.receive(&context);
        assert!(map.texture.is_some());

        // A new session invalidates the old background, even at the same place.
        map.scope = 2;
        map.submit_if_changed(&context, [300, 200]);
        assert!(old_cancel.load(Ordering::Relaxed));
        assert!(map.texture.is_none());
        assert!(map.attribution.is_empty());
        assert!(map.loading);
        let current_generation = map.generation;
        *map.response.lock().unwrap() = Some(completed(old_generation));
        map.receive(&context);
        assert!(map.texture.is_none());
        assert!(map.loading);

        // Dots are supplied to the paint pass, not this background request.
        // Repainting after selection or manual edits reuses the pending view.
        map.submit_if_changed(&context, [300, 200]);
        assert_eq!(map.generation, current_generation);
        assert!(!map.cancel.as_ref().unwrap().load(Ordering::Relaxed));
        *map.response.lock().unwrap() = Some(completed(current_generation));
        map.receive(&context);
        assert!(map.texture.is_some());
        assert!(!map.loading);
        assert_eq!(map.attribution, "test source");
    }

    #[test]
    fn offline_mode_cancels_online_work_and_rejects_late_images() {
        let context = egui::Context::default();
        let mut map = GuiMap::new(&context);
        let cancel = Arc::new(AtomicBool::new(false));
        map.generation = 7;
        map.requested_key = Some(42);
        map.loading = true;
        map.cancel = Some(Arc::clone(&cancel));
        *map.request.lock().unwrap() = Some(RenderRequest {
            generation: 7,
            view: MapView::default(),
            size: [300, 200],
            source: 0,
            cancel: Arc::clone(&cancel),
        });
        map.cancel_online_view();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(map.request.lock().unwrap().is_none());
        assert!(map.requested_key.is_none());
        assert!(!map.loading);

        // A worker may already have passed its final cancellation check when
        // the mode changes. Its old generation still must not be displayed.
        *map.response.lock().unwrap() = Some(RenderResponse {
            generation: 7,
            image: ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]),
            status: "stale online image".into(),
            attribution: "stale attribution".into(),
        });
        map.receive(&context);
        assert!(map.texture.is_none());
        assert!(map.attribution.is_empty());
        let generation = map.generation;
        map.cancel_online_view();
        assert_eq!(map.generation, generation);
    }
}
