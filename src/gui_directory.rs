//! An in-process folder chooser; paths are never reconstructed from list labels.
use std::{fs, path::PathBuf};

use eframe::egui::{self, Color32, Key, RichText, Vec2};

const GREEN: Color32 = Color32::from_rgb(30, 116, 91);
const RED: Color32 = Color32::from_rgb(187, 77, 67);

pub struct DirectoryPicker {
    directory: PathBuf,
    path_input: String,
    directories: Vec<PathBuf>,
    selected: Option<PathBuf>,
    error: Option<String>,
    readable: bool,
    home: Option<PathBuf>,
    initial_focus: bool,
    #[cfg(windows)]
    drives: Vec<PathBuf>,
}

impl DirectoryPicker {
    pub fn new(start: PathBuf) -> Self {
        let directory = if start.is_absolute() {
            start
        } else {
            std::env::current_dir()
                .map(|base| base.join(&start))
                .unwrap_or(start)
        };
        let mut picker = Self {
            path_input: directory.to_string_lossy().into_owned(),
            directory,
            directories: Vec::new(),
            selected: None,
            error: None,
            readable: false,
            home: home_directory(),
            initial_focus: true,
            #[cfg(windows)]
            drives: logical_drives(),
        };
        picker.navigate(picker.directory.clone());
        picker
    }

    /// Returns the chosen folder only after an explicit confirmation click.
    /// The caller owns the surrounding dialog and its cancel button.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        ui.set_max_width(ui.available_width().min(520.0));
        let mut navigate = None;
        ui.horizontal(|ui| {
            let parent = self.directory.parent();
            if ui
                .add_enabled(parent.is_some(), egui::Button::new("上级"))
                .clicked()
            {
                navigate = parent.map(PathBuf::from);
            }
            if ui
                .add_enabled(self.home.is_some(), egui::Button::new("Home"))
                .clicked()
            {
                navigate = self.home.clone();
            }
            if ui.button("刷新").clicked() {
                navigate = Some(self.directory.clone());
            }
            #[cfg(windows)]
            egui::ComboBox::from_id_salt("directory-drives")
                .selected_text("盘符")
                .width(75.0)
                .show_ui(ui, |ui| {
                    for drive in &self.drives {
                        if ui
                            .selectable_label(
                                self.directory.starts_with(drive),
                                drive.display().to_string(),
                            )
                            .clicked()
                        {
                            navigate = Some(drive.clone());
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            let response = ui.add_sized(
                [ui.available_width().max(90.0) - 70.0, 34.0],
                egui::TextEdit::singleline(&mut self.path_input)
                    .id_salt("directory-path")
                    .hint_text("输入文件夹路径，Enter 进入"),
            );
            if self.initial_focus {
                response.request_focus();
                self.initial_focus = false;
            }
            let enter = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if ui.button("前往").clicked() || enter {
                match self.entered_path() {
                    Ok(path) => navigate = Some(path),
                    Err(error) => self.error = Some(error),
                }
            }
        });
        if let Some(path) = navigate.take() {
            self.navigate(path);
        }
        egui::Frame::NONE
            .fill(Color32::from_rgb(247, 249, 246))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(225, 232, 226)))
            .corner_radius(8)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 28.0;
                ui.spacing_mut().button_padding.y = 4.0;
                egui::ScrollArea::vertical()
                    .id_salt(("directory-list", &self.directory))
                    .max_height(168.0)
                    .min_scrolled_height(168.0)
                    .auto_shrink([false, false])
                    .show_rows(ui, 28.0, self.directories.len(), |ui, range| {
                        if self.directories.is_empty() {
                            ui.weak(if self.readable {
                                "此文件夹中没有子文件夹"
                            } else {
                                "无法列出子文件夹"
                            });
                        }
                        for index in range {
                            let path = &self.directories[index];
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            let response = ui
                                .add_sized(
                                    [ui.available_width(), 28.0],
                                    egui::Button::selectable(
                                        self.selected.as_ref() == Some(path),
                                        format!("▸  {name}"),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(path.display().to_string());
                            if response.clicked() {
                                self.selected = Some(path.clone());
                            }
                            if response.double_clicked() {
                                navigate = Some(path.clone());
                            }
                        }
                    });
            });
        if let Some(path) = navigate {
            self.navigate(path);
        }
        let selected = self.selected.as_ref().unwrap_or(&self.directory);
        ui.add(egui::Label::new(format!("已选：{}", selected.display())).truncate())
            .on_hover_text(selected.display().to_string());
        if let Some(error) = &self.error {
            ui.add(egui::Label::new(RichText::new(error).color(RED)).truncate())
                .on_hover_text(error);
        } else {
            ui.weak("单击选择文件夹，双击进入");
        }
        let confirm = ui.add_enabled(
            self.readable,
            egui::Button::new(RichText::new("使用此文件夹").color(Color32::WHITE))
                .fill(GREEN)
                .stroke(egui::Stroke::NONE)
                .min_size(Vec2::new(132.0, 36.0))
                .corner_radius(8),
        );
        if confirm.clicked() {
            let path = selected.clone();
            match fs::read_dir(&path) {
                Ok(_) => return Some(path),
                Err(error) => self.error = Some(format!("无法读取 {}：{error}", path.display())),
            }
        }
        None
    }

    fn entered_path(&self) -> Result<PathBuf, String> {
        // In particular, Enter on a lossy display of a non-UTF-8 path must not
        // replace its original bytes with U+FFFD. List selections stay PathBuf.
        if self.path_input == self.directory.to_string_lossy() {
            return Ok(self.directory.clone());
        }
        if self.path_input.is_empty() {
            return Err("请输入文件夹路径".into());
        }
        if self.path_input == "~" {
            return self
                .home
                .clone()
                .ok_or_else(|| "无法确定 Home 文件夹".into());
        }
        let home_suffix = self.path_input.strip_prefix("~/");
        #[cfg(windows)]
        let home_suffix = home_suffix.or_else(|| self.path_input.strip_prefix("~\\"));
        if let Some(suffix) = home_suffix {
            return self
                .home
                .as_ref()
                .map(|home| home.join(suffix))
                .ok_or_else(|| "无法确定 Home 文件夹".into());
        }
        let path = PathBuf::from(&self.path_input);
        Ok(if path.is_absolute() {
            path
        } else {
            self.directory.join(path)
        })
    }

    fn navigate(&mut self, path: PathBuf) {
        // Resolve explicit `..` components before storing the new location so
        // the next Up click cannot accidentally enter the folder just left.
        let path = if path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        {
            match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    self.error = Some(format!("无法读取 {}：{error}", path.display()));
                    return;
                }
            }
        } else {
            path
        };
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                self.error = Some(format!("无法读取 {}：{error}", path.display()));
                return;
            }
        };
        let mut directories = Vec::new();
        let mut failures = 0;
        for entry in entries {
            match entry {
                Ok(entry) => match entry.file_type() {
                    Ok(kind) if kind.is_dir() => directories.push(entry.path()),
                    Ok(kind) if kind.is_symlink() => match entry.path().metadata() {
                        Ok(metadata) if metadata.is_dir() => directories.push(entry.path()),
                        Ok(_) => (),
                        Err(_) => failures += 1,
                    },
                    Ok(_) => (),
                    Err(_) => failures += 1,
                },
                Err(_) => failures += 1,
            }
        }
        directories.sort_by(|left, right| {
            left.file_name()
                .unwrap_or_default()
                .cmp(right.file_name().unwrap_or_default())
        });
        self.directory = path;
        self.path_input = self.directory.to_string_lossy().into_owned();
        self.directories = directories;
        self.selected = None;
        self.readable = true;
        self.error = (failures > 0).then(|| format!("有 {failures} 个条目无法读取"));
    }
}

fn home_directory() -> Option<PathBuf> {
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| {
        let mut drive = std::env::var_os("HOMEDRIVE")?;
        drive.push(std::env::var_os("HOMEPATH")?);
        Some(drive)
    });
    home.filter(|value| !value.is_empty()).map(PathBuf::from)
}

#[cfg(windows)]
fn logical_drives() -> Vec<PathBuf> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    // Query Windows' drive mask without probing unplugged/removable devices.
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| PathBuf::from(format!("{}:\\", char::from(b'A' + index))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_directories_and_failed_navigation_keeps_selection() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("照片 子目录");
        fs::create_dir(&child).unwrap();
        fs::write(temp.path().join("image.jpg"), []).unwrap();
        let mut picker = DirectoryPicker::new(temp.path().to_owned());
        assert_eq!(picker.directories, vec![child.clone()]);
        picker.selected = Some(child.clone());
        picker.navigate(temp.path().join("missing"));
        assert!(picker.error.as_deref().unwrap().contains("无法读取"));
        assert_eq!(picker.selected, Some(child));
        assert_eq!(picker.directory, temp.path());
    }

    #[test]
    fn relative_input_is_relative_to_browsed_folder_and_preserves_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let mut picker = DirectoryPicker::new(temp.path().to_owned());
        picker.path_input = " 子目录 ".into();
        assert_eq!(picker.entered_path().unwrap(), temp.path().join(" 子目录 "));
        picker.path_input.clear();
        assert!(picker.entered_path().is_err());
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        picker.navigate(child.join(".."));
        assert_eq!(picker.directory, temp.path().canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_folder_keeps_original_path_after_enter() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join(OsString::from_vec(b"photo-\xff".to_vec()));
        fs::create_dir(&child).unwrap();
        let mut picker = DirectoryPicker::new(temp.path().to_owned());
        assert_eq!(picker.directories, vec![child.clone()]);
        picker.navigate(picker.directories[0].clone());
        assert_eq!(picker.directory, child);
        assert_eq!(picker.entered_path().unwrap(), child);
    }
}
