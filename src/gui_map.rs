//! Desktop map panel. Images, tiles and textures remain in memory.
//!
//! There is one pending request and one pending response; a busy map service
//! cannot accumulate a queue of image buffers while the user moves the view.
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
};

use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use photopinfix::map::{self, MapPoint, MapRenderer, MapScale, MapView};

const SCALE_CELL_POINTS: f32 = 8.0;
const MAP_COLOR: Color32 = Color32::from_rgb(18, 24, 34);

struct RenderRequest {
    generation: u64,
    points: Vec<MapPoint>,
    view: MapView,
    size: [u32; 2],
    display_width: f32,
    background: bool,
    source: usize,
    cancel: Arc<AtomicBool>,
}

struct RenderResponse {
    generation: u64,
    image: ColorImage,
    status: String,
    attribution: String,
    scale: Option<MapScale>,
    scale_fraction: f32,
}

/// Call `show` in the map's allocated panel. Set `reset_view` when changing
/// directory/session/scope; selecting another photo should leave it false.
pub struct GuiMap {
    view: MapView,
    fitted: bool,
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
    scale: Option<MapScale>,
    scale_fraction: f32,
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
                // Maintain visible marker sizes on both ordinary and high-DPI
                // displays, including when the image hits its size limit.
                let pixel_ratio = request.size[0] as f32 / request.display_width.max(1.0);
                renderer.set_marker_radii(
                    (3.0 * pixel_ratio).round().max(1.0) as u32,
                    (7.0 * pixel_ratio).round().max(2.0) as u32,
                    (2.0 * pixel_ratio).round().max(1.0) as u32,
                );
                let result = renderer.render_cancellable(
                    &request.points,
                    &request.view,
                    request.size[0],
                    request.size[1],
                    request.background,
                    request.source,
                    &request.cancel,
                );
                if request.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                let rgba = result.image.to_rgba8();
                // Reuse the terminal's metric calculation with virtual cells.
                // The ruler's fraction is carried with this exact image, so
                // asynchronous zoom/resize cannot mismatch the visible scale.
                let columns = (request.display_width / SCALE_CELL_POINTS)
                    .floor()
                    .clamp(1.0, f32::from(u16::MAX)) as u16;
                let scale = request.view.scale(rgba.width(), rgba.height(), columns);
                let scale_fraction = scale
                    .as_ref()
                    .map_or(0.0, |scale| f32::from(scale.cells) / f32::from(columns));
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
                    scale,
                    scale_fraction,
                });
                context.request_repaint();
            }
        });
        Self {
            view: MapView::default(),
            fitted: false,
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
            scale: None,
            scale_fraction: 0.0,
            status: "无底图".into(),
            attribution: String::new(),
            loading: false,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, points: &[MapPoint], reset_view: bool) {
        self.receive(ui.ctx());
        if reset_view || !self.fitted {
            self.view = MapView::fit(points);
            self.fitted = true;
        }
        ui.horizontal_wrapped(|ui| {
            ui.strong("地图");
            ui.selectable_value(&mut self.background, false, "无底图");
            ui.selectable_value(&mut self.background, true, "图片底图");
            if self.background {
                egui::ComboBox::from_id_salt("photopinfix-map-source")
                    .width(180.0)
                    .selected_text(&self.sources[self.source])
                    .show_ui(ui, |ui| {
                        for (index, source) in self.sources.iter().enumerate() {
                            ui.selectable_value(&mut self.source, index, source);
                        }
                    });
            }
            if ui.button("−").on_hover_text("缩小").clicked() {
                self.view.zoom_by(-1.0);
            }
            if ui.button("＋").on_hover_text("放大").clicked() {
                self.view.zoom_by(1.0);
            }
            if ui.button("适配全部点").clicked() {
                self.view = MapView::fit(points);
            }
        });

        let available = ui.available_size();
        // Leave room for one status line. Attribution is painted, wrapping as
        // needed, inside the map and remains visible while a new view loads.
        let size = Vec2::new(available.x.max(1.0), (available.y - 24.0).max(1.0));
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
        self.submit_if_changed(ui.ctx(), points, render_size, size.x);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, MAP_COLOR);
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "正在绘制地图…",
                FontId::proportional(14.0),
                Color32::LIGHT_GRAY,
            );
        }
        if points.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "当前范围没有可显示的 GPS 位置",
                FontId::proportional(14.0),
                Color32::LIGHT_GRAY,
            );
        }
        paint_overlays(
            &painter,
            rect,
            self.scale.as_ref(),
            self.scale_fraction,
            &self.attribution,
        );
        response
            .on_hover_text("滚轮缩放 · 拖动平移 · 绿色为已有 GPS，黄色为待补全 · 大点为当前照片");
        let status = if self.loading {
            format!("更新中… · {}", self.status)
        } else {
            self.status.clone()
        };
        ui.add(egui::Label::new(&status).truncate())
            .on_hover_text(status);
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
        self.scale = response.scale;
        self.scale_fraction = response.scale_fraction;
        self.loading = false;
    }

    fn submit_if_changed(
        &mut self,
        context: &egui::Context,
        points: &[MapPoint],
        size: [u32; 2],
        display_width: f32,
    ) {
        let key = request_key(
            points,
            &self.view,
            size,
            display_width,
            self.background,
            self.source,
        );
        if self.requested_key == Some(key) || self.wake.is_none() {
            return;
        }
        if let Some(previous) = self.cancel.take() {
            previous.store(true, Ordering::Relaxed);
        }
        self.generation = self.generation.wrapping_add(1);
        self.requested_key = Some(key);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        *self
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(RenderRequest {
            generation: self.generation,
            points: points.to_vec(),
            view: self.view.clone(),
            size,
            display_width,
            background: self.background,
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

fn request_key(
    points: &[MapPoint],
    view: &MapView,
    size: [u32; 2],
    display_width: f32,
    background: bool,
    source: usize,
) -> u64 {
    let mut state = DefaultHasher::new();
    for point in points {
        point.coordinates.lat.to_bits().hash(&mut state);
        point.coordinates.lon.to_bits().hash(&mut state);
        point.selected.hash(&mut state);
        point.existing.hash(&mut state);
    }
    for value in [
        view.center_x,
        view.center_y,
        view.span_x,
        view.span_y,
        view.zoom,
    ] {
        value.to_bits().hash(&mut state);
    }
    size.hash(&mut state);
    display_width.to_bits().hash(&mut state);
    background.hash(&mut state);
    source.hash(&mut state);
    state.finish()
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
            FontId::proportional(11.0),
            Color32::WHITE,
            (rect.width() - 18.0).max(1.0),
        );
        let pos = Pos2::new(rect.left() + 9.0, bottom - galley.size().y);
        painter.rect_filled(
            Rect::from_min_size(pos - Vec2::splat(3.0), galley.size() + Vec2::splat(6.0)),
            2.0,
            Color32::from_black_alpha(205),
        );
        painter.galley(pos, galley, Color32::WHITE);
        bottom = pos.y - 12.0;
    }
    if let Some(scale) = scale {
        let width = rect.width() * scale_fraction;
        let start = Pos2::new(rect.left() + 12.0, bottom - 5.0);
        let label = painter.layout_no_wrap(
            scale.label.clone(),
            FontId::proportional(12.0),
            Color32::WHITE,
        );
        let bg = Rect::from_min_max(
            Pos2::new(start.x - 5.0, start.y - label.size().y - 12.0),
            Pos2::new(start.x + width.max(label.size().x) + 5.0, start.y + 9.0),
        );
        painter.rect_filled(bg, 2.0, Color32::from_black_alpha(205));
        let stroke = Stroke::new(1.5, Color32::WHITE);
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
            Color32::WHITE,
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
    fn marker_changes_invalidate_pending_image() {
        let mut points = [point(1.0, 2.0)];
        let view = MapView::fit(&points);
        let key = request_key(&points, &view, [600, 400], 600.0, false, 0);
        points[0].selected = true;
        assert_ne!(
            key,
            request_key(&points, &view, [600, 400], 600.0, false, 0)
        );
        points[0].selected = false;
        points[0].existing = false;
        assert_ne!(
            key,
            request_key(&points, &view, [600, 400], 600.0, false, 0)
        );
    }
}
