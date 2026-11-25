//! Shared type scale: dialog/welcome titles, working controls, and secondary text.
//! Panel headings use the same TEXT size as filenames and controls, with weight
//! for emphasis. Values are logical points; egui applies display DPI once.
pub const TITLE: f32 = 20.0;
pub const TEXT: f32 = 15.0;
pub const META: f32 = 13.0;
pub const MUTED: eframe::egui::Color32 = eframe::egui::Color32::from_rgb(101, 112, 120);
