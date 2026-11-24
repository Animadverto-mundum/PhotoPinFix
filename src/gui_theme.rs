//! Shared type scale for every desktop panel, dialog and painted map label.
//! Values are logical points; egui applies the display's DPI scale once.
pub const TITLE: f32 = 20.0;
pub const TEXT: f32 = 15.0;
pub const META: f32 = 13.0;
pub const MUTED: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(96, 112, 105);
