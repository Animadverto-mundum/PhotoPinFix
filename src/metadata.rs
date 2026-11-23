//! File-format independent metadata access through the embedded exiftool-rs
//! engine (a Rust interpreter with embedded ExifTool modules).
//!
//! Pictures and RAW previews are read through in-memory output buffers. The only temporary files
//! here are the embedded ExifTool schema and a same-directory staging copy used
//! for a verified metadata update; no preview or map image is saved to disk.

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::{self, File, FileTimes, OpenOptions},
    io::{Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use chrono::{FixedOffset, NaiveDateTime, Timelike};
use serde_json::{Map, Value};
use tempfile::NamedTempFile;

use crate::embedded_metadata::{Command, Output};
use crate::model::{Candidate, CaptureTimeInfo, Coordinates, Method, Mode, Photo, Provenance};

const EXIFTOOL_CONFIG: &str = include_str!("../assets/photopinfix-exiftool.config");
const READ_TAGS: &[&str] = &[
    "-q", // Suppress normal "N image files read" progress, retaining warnings.
    "-j",
    "-G1",
    "-n",
    "-a",
    "-FileType",
    "-DateTimeOriginal",
    "-SubSecDateTimeOriginal",
    "-SubSecTimeOriginal",
    "-OffsetTimeOriginal",
    "-GPSLatitude",
    "-GPSLongitude",
    "-GPSLatitudeRef",
    "-GPSLongitudeRef",
    "-GPSCoordinates",
    "-XMP-PhotoPinFix:Mode",
    "-XMP-PhotoPinFix:Method",
    "-Error",
    "-Warning",
];

#[derive(Clone, Debug)]
pub struct MetadataTool {
    config: Arc<NamedTempFile>,
}

#[derive(Debug)]
pub struct ScanResult {
    pub photos: Vec<Photo>,
    pub directories: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
struct Metadata {
    captured_at: Option<NaiveDateTime>,
    capture_time_info: CaptureTimeInfo,
    gps: Option<Coordinates>,
    has_coordinate_tags: bool,
    provenance: Option<Provenance>,
    file_type: String,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    index: u64,
}

#[derive(Debug)]
struct TimeSnapshot {
    accessed: SystemTime,
    modified: SystemTime,
    created: Option<SystemTime>,
    identity: FileIdentity,
    length: u64,
}

impl TimeSnapshot {
    fn capture(file: &File, before: &fs::Metadata) -> Result<Self, String> {
        let current = file.metadata().map_err(|e| e.to_string())?;
        if current.len() != before.len()
            || current.modified().ok() != before.modified().ok()
            || current.created().ok() != before.created().ok()
        {
            return Err("打开照片时文件已经发生变化，请重新扫描".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if current.dev() != before.dev() || current.ino() != before.ino() {
                return Err("打开照片时文件已被替换，请重新扫描".into());
            }
        }
        Ok(Self {
            accessed: before
                .accessed()
                .map_err(|e| format!("无法读取访问时间：{e}"))?,
            modified: before
                .modified()
                .map_err(|e| format!("无法读取修改时间：{e}"))?,
            // Some Unix filesystems do not expose birth time. Keeping the same
            // inode preserves it without inventing a creation time from ctime.
            created: before.created().ok(),
            identity: file_identity(file)?,
            length: before.len(),
        })
    }

    fn check_identity_and_creation(&self, file: &File, path: &Path) -> Result<(), String> {
        let current = file.metadata().map_err(|e| e.to_string())?;
        let path_handle = attribute_handle(path).map_err(|e| e.to_string())?;
        if file_identity(file)? != self.identity || file_identity(&path_handle)? != self.identity {
            return Err("照片文件身份发生变化，已停止写入".into());
        }
        if let Some(created) = self.created
            && current.created().map_err(|e| e.to_string())? != created
        {
            return Err("照片创建时间发生变化，已停止写入".into());
        }
        Ok(())
    }

    fn check_unchanged(&self, file: &File, path: &Path) -> Result<(), String> {
        self.check_identity_and_creation(file, path)?;
        let current = file.metadata().map_err(|e| e.to_string())?;
        if current.len() != self.length
            || current.modified().map_err(|e| e.to_string())? != self.modified
        {
            return Err("照片在处理期间发生变化，已取消写入；请重新扫描".into());
        }
        Ok(())
    }

    fn restore_and_verify(
        &self,
        file: &File,
        path: &Path,
        restore_modified: bool,
    ) -> Result<(), String> {
        self.check_identity_and_creation(file, path)?;
        let mut times = FileTimes::new().set_accessed(self.accessed);
        if restore_modified {
            times = times.set_modified(self.modified);
        }
        file.set_times(times).map_err(|e| e.to_string())?;
        let current = file.metadata().map_err(|e| e.to_string())?;
        if current.accessed().map_err(|e| e.to_string())? != self.accessed {
            return Err("访问时间未能精确恢复".into());
        }
        if restore_modified && current.modified().map_err(|e| e.to_string())? != self.modified {
            return Err("修改时间未能精确恢复".into());
        }
        self.check_identity_and_creation(file, path)
    }
}

/// Restores only access time around read-only ExifTool operations. Modification
/// time is deliberately untouched, so another writer's update is not hidden.
struct AccessGuard {
    path: PathBuf,
    file: File,
    accessed: SystemTime,
    identity: FileIdentity,
    restored: bool,
}

impl AccessGuard {
    fn new(path: &Path) -> Result<Self, String> {
        // Opening an attributes handle performs no content read. Snapshot this
        // handle's metadata so a path replacement between stat and open cannot
        // apply an older file's access time to its replacement.
        let file = attribute_handle(path).map_err(|e| format!("无法保护访问时间：{e}"))?;
        let before = file
            .metadata()
            .map_err(|e| format!("{}：{e}", path.display()))?;
        let accessed = before
            .accessed()
            .map_err(|e| format!("无法读取访问时间：{e}"))?;
        let identity = file_identity(&file)?;
        Ok(Self {
            path: path.to_owned(),
            file,
            accessed,
            identity,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<(), String> {
        let current = attribute_handle(&self.path).map_err(|e| e.to_string())?;
        if file_identity(&current)? != self.identity {
            return Err(format!(
                "读取期间文件已被替换，无法恢复访问时间：{}",
                self.path.display()
            ));
        }
        self.file
            .set_times(FileTimes::new().set_accessed(self.accessed))
            .map_err(|e| format!("无法恢复 {} 的访问时间：{e}", self.path.display()))?;
        if self
            .file
            .metadata()
            .and_then(|metadata| metadata.accessed())
            .map_err(|e| e.to_string())?
            != self.accessed
        {
            return Err(format!("{} 的访问时间回读不一致", self.path.display()));
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for AccessGuard {
    fn drop(&mut self) {
        if !self.restored {
            // The explicit restore in with_preserved_access reports errors;
            // this is only a last attempt during unwinding or early failure.
            let _ = self.restore();
        }
    }
}

fn with_preserved_access<T>(
    paths: &[&Path],
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let mut guards = Vec::with_capacity(paths.len());
    for path in paths {
        guards.push(AccessGuard::new(path)?);
    }
    let result = operation();
    let errors: Vec<String> = guards
        .iter_mut()
        .filter_map(|guard| guard.restore().err())
        .collect();
    if errors.is_empty() {
        result
    } else {
        let original = result
            .err()
            .map(|error| format!("{error}；"))
            .unwrap_or_default();
        Err(format!("{original}{}", errors.join("；")))
    }
}

fn attribute_handle(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES, without content write.
        OpenOptions::new().access_mode(0x0080 | 0x0100).open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

fn file_identity(file: &File) -> Result<FileIdentity, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        Ok(FileIdentity {
            volume: metadata.dev(),
            index: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut u32) -> i32;
        }
        // BY_HANDLE_FILE_INFORMATION consists of thirteen consecutive DWORDs.
        let mut information = [0_u32; 13];
        let success =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
        if success == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(FileIdentity {
            volume: information[7] as u64,
            index: (u64::from(information[11]) << 32) | u64::from(information[12]),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err("此平台不支持文件身份保护".into())
    }
}

impl MetadataTool {
    /// Initialize the compiled-in engine and its private field definitions.
    /// No executable lookup, external interpreter, or system installation is needed.
    pub fn discover() -> Result<Self, String> {
        let mut config = tempfile::Builder::new()
            .prefix("photopinfix-exiftool-")
            .suffix(".config")
            .tempfile()
            .map_err(|e| format!("无法准备 EXIF 字段定义：{e}"))?;
        config
            .write_all(EXIFTOOL_CONFIG.as_bytes())
            .and_then(|_| config.flush())
            .map_err(|e| format!("无法写入 EXIF 字段定义：{e}"))?;
        let tool = Self {
            config: Arc::new(config),
        };
        tool.check_available()
            .map_err(|error| format!("无法初始化内置元数据引擎：{error}"))?;
        Ok(tool)
    }

    fn command(&self) -> Command {
        let mut command = Command::new();
        // -config must precede every other ExifTool option.
        command.arg("-config").arg(self.config.path());
        command.args(["-charset", "filename=UTF8"]);
        command
    }

    fn check_available(&self) -> Result<(), String> {
        let output = self
            .command()
            .arg("-ver")
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(output_error(&output));
        }
        if String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .is_err()
        {
            return Err("程序没有返回有效的 ExifTool 版本".into());
        }
        Ok(())
    }

    /// Reads only this directory. Directories are kept separate for navigation.
    pub fn scan_directory(&self, directory: &Path) -> Result<ScanResult, String> {
        let directory = absolute_path(directory)?;
        let entries = fs::read_dir(&directory)
            .map_err(|e| format!("无法读取目录 {}：{e}", directory.display()))?;
        let mut files = Vec::new();
        let mut directories = Vec::new();
        let mut warnings = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warnings.push(format!("无法读取目录项：{e}"));
                    continue;
                }
            };
            let path = entry.path();
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => directories.push(path),
                Ok(metadata) if metadata.is_file() && is_supported_photo(&path) => files.push(path),
                Ok(_) => {}
                Err(e) => warnings.push(format!("{}：{e}", path.display())),
            }
        }
        files.sort();
        directories.sort();
        let mut photos = Vec::with_capacity(files.len());
        // Batches bound each interpreter's working set and amortize startup.
        let unicode_files: Vec<&PathBuf> = files.iter().filter(|p| p.to_str().is_some()).collect();
        let mut metadata_by_path = HashMap::new();
        for batch in unicode_files.chunks(24) {
            let mut command = self.command();
            command.args(READ_TAGS);
            for path in batch {
                command.arg(path);
            }
            let paths: Vec<&Path> = batch.iter().map(|path| path.as_path()).collect();
            let output = with_preserved_access(&paths, || {
                command
                    .output()
                    .map_err(|e| format!("ExifTool 读取失败：{e}"))
            })?;
            let records: Vec<Value> = serde_json::from_slice(&output.stdout)
                .map_err(|e| format!("ExifTool 返回了无效数据：{e}；{}", output_error(&output)))?;
            for record in records {
                let Some(object) = record.as_object() else {
                    continue;
                };
                let Some(source) = object.get("SourceFile").and_then(Value::as_str) else {
                    continue;
                };
                metadata_by_path.insert(PathBuf::from(source), parse_metadata(object));
            }
            if !output.stderr.is_empty() {
                warnings.push(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            }
        }
        for path in files {
            let parsed = metadata_by_path.remove(&path);
            let photo = match parsed {
                Some(metadata) => Photo {
                    path,
                    captured_at: metadata.captured_at,
                    capture_time_info: metadata.capture_time_info,
                    gps: metadata.gps,
                    provenance: metadata.provenance,
                    candidate: None,
                    error: metadata.error,
                },
                None => {
                    let error = if path.to_str().is_none() {
                        "文件名不是有效 UTF-8；保留文件显示，但无法可靠地关联 ExifTool 元数据"
                            .into()
                    } else {
                        "ExifTool 未返回此文件的元数据".into()
                    };
                    Photo {
                        path,
                        captured_at: None,
                        capture_time_info: CaptureTimeInfo::default(),
                        gps: None,
                        provenance: None,
                        candidate: None,
                        error: Some(error),
                    }
                }
            };
            photos.push(photo);
        }
        Ok(ScanResult {
            photos,
            directories,
            warnings,
        })
    }

    fn read_one(&self, path: &Path) -> Result<Metadata, String> {
        let output = with_preserved_access(&[path], || {
            self.command()
                .args(READ_TAGS)
                .arg(path)
                .output()
                .map_err(|e| format!("无法读取照片元数据：{e}"))
        })?;
        if !output.status.success() {
            return Err(output_error(&output));
        }
        let records: Vec<Value> = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("ExifTool 元数据解析失败：{e}"))?;
        let object = records
            .first()
            .and_then(Value::as_object)
            .ok_or_else(|| "ExifTool 没有返回照片元数据".to_owned())?;
        let metadata = parse_metadata(object);
        if let Some(error) = &metadata.error {
            return Err(error.clone());
        }
        Ok(metadata)
    }

    /// Stage and validate metadata before overwriting the original file through
    /// its existing handle. Keeping the inode preserves creation/birth time.
    /// The original bytes are retained as <filename>.photopinfix-original.
    pub fn write_gps(&self, path: &Path, candidate: &Candidate) -> Result<(), String> {
        if !valid_coordinates(candidate.coordinates) {
            return Err("GPS 坐标超出范围或不是有限数字".into());
        }
        if path.to_str().is_none() {
            return Err("此文件名不是有效 UTF-8，无法安全写入元数据".into());
        }
        let path = absolute_path(path)?;
        let source_info = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if source_info.file_type().is_symlink() {
            return Err("请打开符号链接的实际文件所在目录后再写入".into());
        }
        if !source_info.is_file() || source_info.permissions().readonly() {
            return Err("目标不是可写的普通文件".into());
        }
        // Capture before the first content read. All exit paths below restore
        // these times, including already-tagged files and failed staging.
        // Keep a read-only handle to the original inode while the embedded
        // engine works. Its Windows Exists check shares reads, not writes.
        let mut source = File::open(&path).map_err(|e| format!("无法打开照片进行写入：{e}"))?;
        let snapshot = TimeSnapshot::capture(&source, &source_info)?;
        let mut original_changed = false;
        let mut backup_path = None;
        let result = (|| -> Result<(), String> {
            snapshot.check_unchanged(&source, &path)?;
            let before = self.read_one(&path)?;
            if before.has_coordinate_tags {
                return Err("照片已有 GPS 字段，已跳过写入；请重新扫描目录".into());
            }
            let parent = path
                .parent()
                .ok_or_else(|| "照片没有有效的父目录".to_owned())?;
            let extension = path.extension().and_then(OsStr::to_str).unwrap_or("img");
            let mut staged = tempfile::Builder::new()
                .prefix(".photopinfix-")
                .suffix(&format!(".{extension}"))
                .tempfile_in(parent)
                .map_err(|e| format!("无法在照片目录准备写入副本：{e}"))?;
            source.rewind().map_err(|e| e.to_string())?;
            std::io::copy(&mut source, staged.as_file_mut()).map_err(|e| e.to_string())?;
            staged.as_file_mut().flush().map_err(|e| e.to_string())?;
            // Close the writable handle before the engine checks, rewrites or
            // renames this file. TempPath still cleans up on every exit path.
            let staged = staged.into_temp_path();
            let staged_path: &Path = staged.as_ref();

            let coordinates = candidate.coordinates;
            let mut command = self.command();
            command.args(["-overwrite_original", "-P"]);
            // CRW can carry XMP coordinates but has no writable EXIF GPS IFD.
            if before.file_type != "CRW" {
                command.arg(format!("-EXIF:GPSLatitude={:.10}", coordinates.lat.abs()));
                command.arg(format!(
                    "-EXIF:GPSLatitudeRef={}",
                    if coordinates.lat < 0.0 { "S" } else { "N" }
                ));
                command.arg(format!("-EXIF:GPSLongitude={:.10}", coordinates.lon.abs()));
                command.arg(format!(
                    "-EXIF:GPSLongitudeRef={}",
                    if coordinates.lon < 0.0 { "W" } else { "E" }
                ));
            }
            command.arg(format!("-XMP-exif:GPSLatitude={:.10}", coordinates.lat));
            command.arg(format!("-XMP-exif:GPSLongitude={:.10}", coordinates.lon));
            command.arg(format!(
                "-XMP-PhotoPinFix:Mode={}",
                mode_name(candidate.provenance.mode)
            ));
            command.arg(format!(
                "-XMP-PhotoPinFix:Method={}",
                method_name(candidate.provenance.method)
            ));
            command.arg(staged_path);
            let output = command
                .output()
                .map_err(|e| format!("ExifTool 写入失败：{e}"))?;
            if !output.status.success() {
                return Err(format!("原文件未修改：{}", output_error(&output)));
            }
            let after = self.read_one(staged_path)?;
            verify_update(&before, &after, candidate)?;
            snapshot.check_unchanged(&source, &path)?;
            if self.read_one(&path)?.has_coordinate_tags {
                return Err("照片在处理期间已有 GPS，已取消写入；请重新扫描".into());
            }

            let mut backup_name = path.file_name().unwrap_or_default().to_os_string();
            backup_name.push(".photopinfix-original");
            let backup = parent.join(backup_name);
            let mut backup_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)
                .map_err(|e| format!("无法创建原图备份 {}：{e}；原文件未修改", backup.display()))?;
            let backup_result = (|| -> std::io::Result<()> {
                source.rewind()?;
                std::io::copy(&mut source, &mut backup_file)?;
                backup_file.sync_all()
            })();
            drop(backup_file);
            if let Err(error) = backup_result {
                let _ = fs::remove_file(&backup);
                return Err(format!("原图备份失败，原文件未修改：{error}"));
            }
            backup_path = Some(backup);
            // Check identity, size and modification time after backup too.
            snapshot.check_unchanged(&source, &path)?;
            // ExifTool replaced the staged inode, so reopen its pathname. Never
            // rename this copy over the original: that would change birth time.
            let mut staged_file = File::open(staged_path).map_err(|e| e.to_string())?;
            let staged_length = staged_file.metadata().map_err(|e| e.to_string())?.len();
            let mut writer = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| format!("无法打开照片进行写入：{e}"))?;
            // Revalidate the newly opened writer, not just the retained guard,
            // so replacing the pathname cannot redirect the commit.
            snapshot.check_unchanged(&writer, &path)?;
            writer.rewind().map_err(|e| e.to_string())?;
            original_changed = true;
            std::io::copy(&mut staged_file, &mut writer)
                .map_err(|e| format!("写入照片失败：{e}"))?;
            writer.set_len(staged_length).map_err(|e| e.to_string())?;
            writer.sync_all().map_err(|e| e.to_string())?;
            snapshot.check_identity_and_creation(&writer, &path)?;
            drop(writer);
            // Validate the actual original, before the final timestamp restore.
            let committed = self.read_one(&path)?;
            verify_update(&before, &committed, candidate)?;
            Ok(())
        })();

        let mut problems = Vec::new();
        if let Err(error) = result {
            problems.push(error);
        }
        if !problems.is_empty()
            && original_changed
            && let Some(backup) = &backup_path
        {
            let rollback = (|| -> Result<(), String> {
                snapshot.check_identity_and_creation(&source, &path)?;
                let mut original = File::open(backup).map_err(|e| e.to_string())?;
                let mut writer = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                snapshot.check_identity_and_creation(&writer, &path)?;
                writer.rewind().map_err(|e| e.to_string())?;
                std::io::copy(&mut original, &mut writer).map_err(|e| e.to_string())?;
                writer.set_len(snapshot.length).map_err(|e| e.to_string())?;
                writer.sync_all().map_err(|e| e.to_string())?;
                snapshot.check_identity_and_creation(&writer, &path)?;
                Ok(())
            })();
            match rollback {
                Ok(()) => problems.push("已从备份恢复原文件内容".into()),
                Err(error) => problems.push(format!(
                    "恢复原文件失败：{error}；原始备份位于 {}",
                    backup.display()
                )),
            }
        }
        // A concurrent external edit detected before our first write must keep
        // its own modification time. Reads restore access time only.
        let restore_modified = original_changed
            || source
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                == Some(snapshot.modified);
        // Close the remaining content-reading handle before restoring times.
        // Windows may defer access-time updates until that handle closes.
        drop(source);
        match attribute_handle(&path) {
            Ok(time_handle) => {
                if let Err(error) =
                    snapshot.restore_and_verify(&time_handle, &path, restore_modified)
                {
                    problems.push(format!("文件时间恢复失败：{error}"));
                }
            }
            Err(error) => problems.push(format!("无法打开文件恢复时间：{error}")),
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("；"))
        }
    }

    /// Returns one embedded image, never concatenated previews or a temp file.
    pub fn preview_bytes(&self, path: &Path) -> Result<Vec<u8>, String> {
        let path = absolute_path(path)?;
        with_preserved_access(&[&path], || {
            let mut last_error = None;
            for tag in [
                "-JpgFromRaw",
                "-PreviewImage",
                "-OtherImage",
                "-ThumbnailImage",
            ] {
                let output = self
                    .command()
                    .arg("-b")
                    .arg(tag)
                    .arg(&path)
                    .output()
                    .map_err(|e| format!("提取预览失败：{e}"))?;
                if output.status.success() && !output.stdout.is_empty() {
                    return Ok(output.stdout);
                }
                if !output.status.success() {
                    last_error = Some(output_error(&output));
                }
            }
            Err(last_error.unwrap_or_else(|| "此图片没有可读取的内嵌预览图".into()))
        })
    }

    /// The main image's EXIF orientation, for embedded RAW previews whose JPEG
    /// does not repeat the orientation tag. Unknown/missing means upright (1).
    pub fn orientation(&self, path: &Path) -> Result<u8, String> {
        let path = absolute_path(path)?;
        let output = with_preserved_access(&[&path], || {
            self.command()
                .args(["-s3", "-n", "-Orientation"])
                .arg(&path)
                .output()
                .map_err(|e| format!("读取图片方向失败：{e}"))
        })?;
        if !output.status.success() {
            return Err(output_error(&output));
        }
        let orientation = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u8>()
            .ok();
        Ok(orientation
            .filter(|value| (1..=8).contains(value))
            .unwrap_or(1))
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| e.to_string())
    }
}

pub fn is_supported_photo(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(OsStr::to_str) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "jpg"
            | "jpeg"
            | "jpe"
            | "png"
            | "heif"
            | "heic"
            | "hif"
            | "avif"
            | "tif"
            | "tiff"
            | "webp"
            | "jxl"
            | "bmp"
            | "gif"
            | "arw"
            | "arq"
            | "sr2"
            | "srf"
            | "cr2"
            | "cr3"
            | "crw"
            | "dng"
            | "rwl"
            | "rw2"
            | "nef"
            | "nrw"
            | "raf"
            | "orf"
            | "ori"
            | "pef"
            | "ptx"
            | "srw"
            | "3fr"
            | "fff"
            | "iiq"
            | "erf"
            | "kdc"
            | "dcr"
            | "mos"
            | "mrw"
            | "x3f"
            | "raw"
    )
}

fn parse_metadata(object: &Map<String, Value>) -> Metadata {
    let date_text = value_string(
        object,
        &[
            "Composite:SubSecDateTimeOriginal",
            "ExifIFD:DateTimeOriginal",
            "XMP-exif:DateTimeOriginal",
            "QuickTime:DateTimeOriginal",
            "Composite:DateTimeOriginal",
            "DateTimeOriginal",
        ],
    );
    let subsecond = value_string(
        object,
        &["ExifIFD:SubSecTimeOriginal", "SubSecTimeOriginal"],
    );
    let offset = value_string(
        object,
        &[
            "ExifIFD:OffsetTimeOriginal",
            "XMP-exifEX:OffsetTimeOriginal",
            "OffsetTimeOriginal",
        ],
    );
    let (captured_at, capture_time_info) = date_text
        .as_deref()
        .and_then(|date| parse_capture_time(date, subsecond.as_deref(), offset.as_deref()))
        .map(|(time, info)| (Some(time), info))
        .unwrap_or_default();
    let gps = parse_gps(object);
    let has_coordinate_tags = object.keys().any(|key| {
        matches!(
            key.rsplit(':').next(),
            Some("GPSLatitude" | "GPSLongitude" | "GPSCoordinates")
        )
    });
    let mode = match value_string(object, &["XMP-PhotoPinFix:Mode"]).as_deref() {
        Some("auto") => Some(Mode::Auto),
        Some("manual") => Some(Mode::Manual),
        _ => None,
    };
    let method = match value_string(object, &["XMP-PhotoPinFix:Method"]).as_deref() {
        Some("time-interpolation") => Some(Method::TimeInterpolation),
        Some("copy-previous") => Some(Method::CopyPrevious),
        Some("copy-next") => Some(Method::CopyNext),
        Some("copy-nearest") => Some(Method::CopyNearest),
        Some("coordinate-input") => Some(Method::CoordinateInput),
        _ => None,
    };
    let provenance = mode
        .zip(method)
        .map(|(mode, method)| Provenance { mode, method });
    let file_type = value_string(object, &["File:FileType", "FileType"]).unwrap_or_default();
    let mut error = value_string(object, &["ExifTool:Error", "Error"]);
    if error.is_none() && has_coordinate_tags && gps.is_none() {
        error = Some("GPS 字段不完整或无效，需人工检查；不会覆盖现有字段".into());
    }
    Metadata {
        captured_at,
        capture_time_info,
        gps,
        has_coordinate_tags,
        provenance,
        file_type,
        error,
    }
}

fn value_string(object: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        object.get(*name).and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_number().map(ToString::to_string))
        })
    })
}

fn parse_capture_time(
    text: &str,
    subsecond: Option<&str>,
    offset: Option<&str>,
) -> Option<(NaiveDateTime, CaptureTimeInfo)> {
    // Preserve the camera's local capture clock: no assumption that a missing
    // OffsetTimeOriginal is the computer's timezone or UTC.
    let base = text.get(..19)?;
    let mut time = [
        "%Y:%m:%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(base, format).ok())?;
    let suffix = text.get(19..)?.trim();
    let (inline_fraction, inline_offset) = if let Some(fraction) = suffix.strip_prefix('.') {
        let count = fraction.bytes().take_while(u8::is_ascii_digit).count();
        (
            fraction.get(..count),
            fraction.get(count..).unwrap_or_default().trim(),
        )
    } else {
        (None, suffix)
    };
    let subsecond_digits = inline_fraction
        .filter(|digits| !digits.is_empty())
        .or(subsecond)
        .map(|fraction| {
            fraction
                .trim()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|digits| !digits.is_empty());
    if let Some(digits) = &subsecond_digits {
        let nanos_digits = &digits[..digits.len().min(9)];
        let nanos = nanos_digits.parse::<u32>().ok()? * 10_u32.pow(9 - nanos_digits.len() as u32);
        time = time.with_nanosecond(nanos)?;
    }
    let offset =
        parse_capture_offset(inline_offset).or_else(|| offset.and_then(parse_capture_offset));
    Some((
        time,
        CaptureTimeInfo {
            offset,
            subsecond_digits,
        },
    ))
}

fn parse_capture_offset(text: &str) -> Option<FixedOffset> {
    let text = text.trim();
    if text == "Z" || text == "z" {
        return FixedOffset::east_opt(0);
    }
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = text.get(1..)?;
    let (hours, minutes) = match digits.len() {
        5 if digits.as_bytes()[2] == b':' => (digits.get(..2)?, digits.get(3..)?),
        4 => (digits.get(..2)?, digits.get(2..)?),
        2 => (digits, "00"),
        _ => return None,
    };
    if !hours
        .bytes()
        .chain(minutes.bytes())
        .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let hours = hours.parse::<i32>().ok()?;
    let minutes = minutes.parse::<i32>().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    // RFC 3339's negative zero explicitly means an unknown local offset.
    if sign == -1 && hours == 0 && minutes == 0 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

fn parse_gps(object: &Map<String, Value>) -> Option<Coordinates> {
    let mut groups = vec![
        "GPS".to_owned(),
        "XMP-exif".to_owned(),
        "Composite".to_owned(),
    ];
    for key in object.keys() {
        if let Some(prefix) = key.strip_suffix(":GPSLatitude")
            && !groups.iter().any(|group| group == prefix)
        {
            groups.push(prefix.to_owned());
        }
    }
    for group in groups {
        let lat_key = format!("{group}:GPSLatitude");
        let lon_key = format!("{group}:GPSLongitude");
        if let (Some(mut lat), Some(mut lon)) =
            (number(object.get(&lat_key)), number(object.get(&lon_key)))
        {
            let lat_ref = value_string(object, &[&format!("{group}:GPSLatitudeRef")]);
            let lon_ref = value_string(object, &[&format!("{group}:GPSLongitudeRef")]);
            if let Some(reference) = lat_ref {
                lat = if reference.starts_with('S') {
                    -lat.abs()
                } else {
                    lat.abs()
                };
            }
            if let Some(reference) = lon_ref {
                lon = if reference.starts_with('W') {
                    -lon.abs()
                } else {
                    lon.abs()
                };
            }
            let coordinates = Coordinates { lat, lon };
            if valid_coordinates(coordinates) {
                return Some(coordinates);
            }
        }
    }
    if let (Some(lat), Some(lon)) = (
        number(object.get("GPSLatitude")),
        number(object.get("GPSLongitude")),
    ) {
        let coordinates = Coordinates { lat, lon };
        if valid_coordinates(coordinates) {
            return Some(coordinates);
        }
    }
    for (key, value) in object {
        if key.rsplit(':').next() == Some("GPSCoordinates")
            && let Some(text) = value.as_str()
            && let Some(coordinates) = parse_iso6709(text)
        {
            return Some(coordinates);
        }
    }
    None
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn parse_iso6709(text: &str) -> Option<Coordinates> {
    let text = text.trim();
    let split = text
        .char_indices()
        .skip(1)
        .find(|(_, character)| matches!(character, '+' | '-'))
        .map(|(index, _)| index)?;
    let lat = text[..split].parse().ok()?;
    let longitude = &text[split..];
    let end = longitude
        .char_indices()
        .skip(1)
        .find(|(_, character)| matches!(character, '+' | '-' | '/'))
        .map(|(index, _)| index)
        .unwrap_or(longitude.len());
    let lon = longitude[..end].parse().ok()?;
    let coordinates = Coordinates { lat, lon };
    valid_coordinates(coordinates).then_some(coordinates)
}

fn valid_coordinates(coordinates: Coordinates) -> bool {
    coordinates.lat.is_finite()
        && coordinates.lon.is_finite()
        && (-90.0..=90.0).contains(&coordinates.lat)
        && (-180.0..=180.0).contains(&coordinates.lon)
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Auto => "auto",
        Mode::Manual => "manual",
    }
}

fn method_name(method: Method) -> &'static str {
    match method {
        Method::TimeInterpolation => "time-interpolation",
        Method::CopyPrevious => "copy-previous",
        Method::CopyNext => "copy-next",
        Method::CopyNearest => "copy-nearest",
        Method::CoordinateInput => "coordinate-input",
    }
}

fn verify_update(before: &Metadata, after: &Metadata, candidate: &Candidate) -> Result<(), String> {
    let Some(actual) = after.gps else {
        return Err("写入副本未能读取到 GPS，原文件未修改".into());
    };
    if (actual.lat - candidate.coordinates.lat).abs() > 1e-7
        || (actual.lon - candidate.coordinates.lon).abs() > 1e-7
    {
        return Err("写入坐标回读不一致，原文件未修改".into());
    }
    let Some(provenance) = &after.provenance else {
        return Err("写入副本缺少 PhotoPinFix 来源字段，原文件未修改".into());
    };
    if mode_name(provenance.mode) != mode_name(candidate.provenance.mode)
        || method_name(provenance.method) != method_name(candidate.provenance.method)
    {
        return Err("PhotoPinFix 来源字段回读不一致，原文件未修改".into());
    }
    if before.captured_at != after.captured_at
        || before.capture_time_info != after.capture_time_info
    {
        return Err("写入副本的拍摄时间发生变化，原文件未修改".into());
    }
    Ok(())
}

fn output_error(output: &Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !message.is_empty() {
        return message;
    }
    let message = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !message.is_empty() {
        return message.chars().take(800).collect();
    }
    format!("ExifTool 退出状态：{}", output.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capture_time_uses_subseconds_and_does_not_shift_unknown_camera_time() {
        let (time, info) = parse_capture_time("2026:11:22 14:25:30.125+08:00", None, None).unwrap();
        assert_eq!(
            time.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            "2026-11-22 14:25:30.125"
        );
        assert_eq!(info.offset.unwrap().local_minus_utc(), 8 * 3600);
        assert_eq!(info.subsecond_digits.as_deref(), Some("125"));
        assert_eq!(
            parse_capture_time("2026:11:22 14:25:30", Some("5"), None)
                .unwrap()
                .0
                .nanosecond(),
            500_000_000
        );
        assert!(parse_capture_time("0000:00:00 00:00:00", None, None).is_none());
    }

    fn photo_from_tags(value: Value) -> Photo {
        let metadata = parse_metadata(value.as_object().unwrap());
        Photo {
            path: "timestamp-fixture.jpg".into(),
            captured_at: metadata.captured_at,
            capture_time_info: metadata.capture_time_info,
            gps: metadata.gps,
            provenance: metadata.provenance,
            candidate: None,
            error: metadata.error,
        }
    }

    #[test]
    fn capture_labels_preserve_local_time_offsets_and_all_fraction_digits() {
        for (offset, seconds) in [("+08:00", 28800), ("-03:30", -12600), ("+05:45", 20700)] {
            let photo = photo_from_tags(json!({
                "ExifIFD:DateTimeOriginal": "2026:11:22 14:25:30",
                "ExifIFD:SubSecTimeOriginal": "000",
                "ExifIFD:OffsetTimeOriginal": offset,
            }));
            assert_eq!(photo.captured_at.unwrap().hour(), 14);
            assert_eq!(
                photo.capture_time_info.offset.unwrap().local_minus_utc(),
                seconds
            );
            assert_eq!(
                photo.capture_time_label(),
                format!("2026-11-22 14:25:30.000 {offset}")
            );
        }
        let photo = photo_from_tags(json!({
            "Composite:SubSecDateTimeOriginal": "2026:11:22 14:25:30.123456789012300+08:00",
            "ExifIFD:SubSecTimeOriginal": "123456789012300",
            "ExifIFD:OffsetTimeOriginal": "+08:00",
        }));
        assert_eq!(photo.captured_at.unwrap().nanosecond(), 123_456_789);
        assert_eq!(
            photo.capture_time_label(),
            "2026-11-22 14:25:30.123456789012300 +08:00"
        );
        let photo = photo_from_tags(json!({
            "XMP-exif:DateTimeOriginal": "2026-11-22T14:25:30.0500Z",
        }));
        assert_eq!(
            photo.capture_time_label(),
            "2026-11-22 14:25:30.0500 +00:00"
        );
    }

    #[test]
    fn missing_or_invalid_offset_is_explicitly_unknown() {
        for offset in [
            None,
            Some("+25:00"),
            Some("+08:99"),
            Some("-00:00"),
            Some("local"),
        ] {
            let photo = photo_from_tags(json!({
                "ExifIFD:DateTimeOriginal": "2026:11:22 14:25:30",
                "ExifIFD:OffsetTimeOriginal": offset,
            }));
            assert!(photo.capture_time_info.offset.is_none());
            assert_eq!(photo.capture_time_label(), "2026-11-22 14:25:30 [时区未知]");
        }
        let photo = photo_from_tags(json!({
            "QuickTime:DateTimeOriginal": "2026:11:22 14:25:30-0700",
        }));
        assert_eq!(photo.capture_time_label(), "2026-11-22 14:25:30 -07:00");
        assert_eq!(
            photo_from_tags(json!({})).capture_time_label(),
            "拍摄时间未知"
        );
    }

    #[test]
    fn southern_and_western_exif_coordinates_preserve_sign() {
        let value = json!({"GPS:GPSLatitude": 35.5, "GPS:GPSLatitudeRef": "S", "GPS:GPSLongitude": 120.25, "GPS:GPSLongitudeRef": "W"});
        let result = parse_metadata(value.as_object().unwrap());
        assert_eq!(
            result.gps.unwrap(),
            Coordinates {
                lat: -35.5,
                lon: -120.25
            }
        );
    }

    #[test]
    fn xmp_only_gps_is_existing_gps_including_zero_coordinates() {
        let value = json!({"XMP-exif:GPSLatitude": 0, "XMP-exif:GPSLongitude": 0});
        let result = parse_metadata(value.as_object().unwrap());
        assert!(result.has_coordinate_tags);
        assert_eq!(result.gps.unwrap(), Coordinates { lat: 0.0, lon: 0.0 });
    }

    #[test]
    fn incomplete_gps_is_reported_and_never_treated_as_safe_to_overwrite() {
        let value = json!({"GPS:GPSLatitude": 20});
        let result = parse_metadata(value.as_object().unwrap());
        assert!(result.has_coordinate_tags);
        assert!(result.gps.is_none());
        assert!(result.error.is_some());
    }

    #[test]
    fn provenance_needs_both_known_values() {
        let value = json!({"XMP-PhotoPinFix:Mode": "manual", "XMP-PhotoPinFix:Method": "time-interpolation"});
        let result = parse_metadata(value.as_object().unwrap());
        assert_eq!(
            result.provenance.as_ref().map(|p| mode_name(p.mode)),
            Some("manual")
        );
        let unknown = json!({"XMP-PhotoPinFix:Mode": "native", "XMP-PhotoPinFix:Method": "time-interpolation"});
        assert!(
            parse_metadata(unknown.as_object().unwrap())
                .provenance
                .is_none()
        );
    }

    #[test]
    fn iso6709_handles_signed_coordinates_and_altitude() {
        assert_eq!(
            parse_iso6709("+35.1234-120.5000+10.0/").unwrap(),
            Coordinates {
                lat: 35.1234,
                lon: -120.5
            }
        );
        assert!(parse_iso6709("+95.0+120.0/").is_none());
    }

    #[test]
    fn photo_types_are_case_insensitive_and_do_not_include_backups() {
        assert!(is_supported_photo(Path::new("test.CR3")));
        assert!(is_supported_photo(Path::new("test.HEIF")));
        assert!(!is_supported_photo(Path::new(
            "test.CR3.photopinfix-original"
        )));
    }

    #[test]
    fn embedded_round_trip_preserves_capture_time_and_original_and_refuses_existing_gps() {
        let tool = MetadataTool::discover().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("照片 with spaces.png");
        let png = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\x68\x68\x68\x00\x00\x03\x04\x01\x81\x4b\xd3\xd2\x10\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82";
        fs::write(&path, png).unwrap();
        let output = tool
            .command()
            .args([
                "-overwrite_original",
                "-DateTimeOriginal=2026:11:22 10:12:13",
                "-SubSecTimeOriginal=050",
                "-OffsetTimeOriginal=+05:45",
                "-Artist=PhotoPinFix fixture",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", output_error(&output));
        let original = fs::read(&path).unwrap();
        let candidate = Candidate {
            coordinates: Coordinates {
                lat: -35.1234567,
                lon: -120.7654321,
            },
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
        };
        tool.write_gps(&path, &candidate).unwrap();
        let metadata = tool.read_one(&path).unwrap();
        assert!((metadata.gps.unwrap().lat - candidate.coordinates.lat).abs() < 1e-7);
        assert_eq!(metadata.provenance.unwrap().mode, Mode::Manual);
        assert_eq!(metadata.captured_at.unwrap().nanosecond(), 50_000_000);
        assert_eq!(
            metadata.capture_time_info.subsecond_digits.as_deref(),
            Some("050")
        );
        assert_eq!(
            metadata.capture_time_info.offset.unwrap().local_minus_utc(),
            20700
        );
        assert_eq!(
            fs::read(
                directory
                    .path()
                    .join("照片 with spaces.png.photopinfix-original")
            )
            .unwrap(),
            original
        );
        let artist = tool
            .command()
            .args(["-s3", "-Artist"])
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&artist.stdout).trim(),
            "PhotoPinFix fixture"
        );
        let scan = tool.scan_directory(directory.path()).unwrap();
        assert_eq!(scan.photos.len(), 1);
        assert!(scan.photos[0].gps.is_some());
        assert_eq!(
            scan.photos[0].capture_time_label(),
            "2026-11-22 10:12:13.050 +05:45"
        );
        let written = fs::read(&path).unwrap();
        assert!(tool.write_gps(&path, &candidate).is_err());
        assert_eq!(fs::read(&path).unwrap(), written);
    }

    fn set_fixture_times(path: &Path) -> TimeSnapshot {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let accessed =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_500_000_000, 123_456_700);
        let modified =
            SystemTime::UNIX_EPOCH + std::time::Duration::new(1_600_000_000, 987_654_300);
        file.set_times(
            FileTimes::new()
                .set_accessed(accessed)
                .set_modified(modified),
        )
        .unwrap();
        drop(file);
        let metadata = fs::metadata(path).unwrap();
        let handle = attribute_handle(path).unwrap();
        TimeSnapshot::capture(&handle, &metadata).unwrap()
    }

    fn assert_fixture_times(path: &Path, before: &TimeSnapshot) {
        let handle = attribute_handle(path).unwrap();
        let after = handle.metadata().unwrap();
        assert_eq!(
            after.accessed().unwrap(),
            before.accessed,
            "access time changed"
        );
        assert_eq!(
            after.modified().unwrap(),
            before.modified,
            "modification time changed"
        );
        assert_eq!(
            after.created().ok(),
            before.created,
            "creation/birth time changed"
        );
        assert_eq!(
            file_identity(&handle).unwrap(),
            before.identity,
            "original file was replaced"
        );
    }

    #[test]
    fn access_guard_preserves_external_modification_and_rejects_path_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guard.bin");
        fs::write(&path, b"original content").unwrap();
        let before = set_fixture_times(&path);
        let mut guard = AccessGuard::new(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original content");
        let changed_modified = before.modified + std::time::Duration::from_secs(60);
        attribute_handle(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(changed_modified))
            .unwrap();
        guard.restore().unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.accessed().unwrap(), before.accessed);
        assert_eq!(after.modified().unwrap(), changed_modified);
        assert_eq!(after.created().ok(), before.created);
        drop(guard);

        let mut guard = AccessGuard::new(&path).unwrap();
        fs::rename(&path, directory.path().join("moved.bin")).unwrap();
        fs::write(&path, b"replacement content").unwrap();
        let replacement = set_fixture_times(&path);
        assert!(guard.restore().is_err());
        drop(guard);
        assert_fixture_times(&path, &replacement);
    }

    #[test]
    fn jpeg_and_png_keep_creation_modification_access_times_and_file_identity() {
        let tool = MetadataTool::discover().unwrap();
        for extension in ["jpg", "png"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("photo.{extension}"));
            image::RgbImage::from_pixel(4, 4, image::Rgb([100, 150, 200]))
                .save(&path)
                .unwrap();
            let before = set_fixture_times(&path);
            let candidate = Candidate {
                coordinates: Coordinates {
                    lat: 31.1234567,
                    lon: 121.7654321,
                },
                provenance: Provenance {
                    mode: Mode::Auto,
                    method: Method::TimeInterpolation,
                },
            };
            tool.write_gps(&path, &candidate).unwrap();
            assert_fixture_times(&path, &before);
            assert!(tool.read_one(&path).unwrap().gps.is_some());
            assert_fixture_times(&path, &before);
            let scanned = tool.scan_directory(directory.path()).unwrap();
            assert_eq!(scanned.photos.len(), 1);
            assert!(scanned.warnings.is_empty(), "{:?}", scanned.warnings);
            assert_fixture_times(&path, &before);
            let _ = tool.preview_bytes(&path); // No preview is also a guarded read path.
            assert_fixture_times(&path, &before);
            assert_eq!(tool.orientation(&path).unwrap(), 1);
            assert_fixture_times(&path, &before);
            let content =
                with_preserved_access(&[&path], || fs::read(&path).map_err(|e| e.to_string()))
                    .unwrap();
            assert!(tool.write_gps(&path, &candidate).is_err());
            assert_fixture_times(&path, &before);
            let unchanged =
                with_preserved_access(&[&path], || fs::read(&path).map_err(|e| e.to_string()))
                    .unwrap();
            assert_eq!(content, unchanged);
            assert_fixture_times(&path, &before);
        }
    }

    #[test]
    fn staging_failure_also_restores_original_times_and_bytes() {
        let tool = MetadataTool::discover().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.png");
        image::RgbImage::from_pixel(4, 4, image::Rgb([100, 150, 200]))
            .save(&path)
            .unwrap();
        let content = fs::read(&path).unwrap();
        let before = set_fixture_times(&path);
        fs::write(
            directory.path().join("photo.png.photopinfix-original"),
            b"existing backup",
        )
        .unwrap();
        let candidate = Candidate {
            coordinates: Coordinates {
                lat: 31.0,
                lon: 121.0,
            },
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
        };
        assert!(tool.write_gps(&path, &candidate).is_err());
        assert_fixture_times(&path, &before);
        let after =
            with_preserved_access(&[&path], || fs::read(&path).map_err(|e| e.to_string())).unwrap();
        assert_eq!(after, content);
        assert_fixture_times(&path, &before);
    }
}
