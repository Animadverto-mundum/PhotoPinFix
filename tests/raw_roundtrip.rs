//! Real camera RAW regression coverage. The public demo files are never edited;
//! every write operates on a fresh temporary copy and keeps its original inode.
//!
//! Run with `PHOTOPINFIX_REQUIRE_RAW=1 cargo test --test raw_roundtrip -- --nocapture`
//! to require all nine formats and all eight camera manufacturers. A checkout
//! without the optional large demo/raw download skips this test by default.
//! External ExifTool is only an independent development oracle: when absent,
//! non-strict runs skip this test; standalone_metadata still tests native I/O.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, FileTimes, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    time::{Duration, SystemTime},
};

use photopinfix::{
    metadata::MetadataTool,
    model::{Candidate, Coordinates, Method, Mode, Photo, Provenance, Settings},
    preview::load_preview,
    strategy::{build_sessions, recompute_candidates, sort_photos},
};
use serde_json::Value;

const REQUIRED_FORMATS: &[&str] = &[
    "ARW", "CR2", "CR3", "DNG", "NEF", "RAF", "RW2", "ORF", "PEF",
];
const REQUIRED_MAKERS: &[&str] = &[
    "sony",
    "canon",
    "leica",
    "nikon",
    "fujifilm",
    "panasonic",
    "olympus",
    "pentax",
];

#[derive(Clone)]
struct ExifReader {
    program: OsString,
    prefix: Vec<OsString>,
    config: PathBuf,
}

impl ExifReader {
    fn discover(root: &Path) -> Result<Self, String> {
        let config = root.join("assets/photopinfix-exiftool.config");
        let candidates = if let Some(program) = std::env::var_os("PHOTOPINFIX_EXIFTOOL") {
            vec![program]
        } else {
            vec![
                OsString::from("exiftool"),
                root.join("tools/exiftool/exiftool").into_os_string(),
                root.join("tools/exiftool/exiftool.exe").into_os_string(),
            ]
        };
        for candidate in candidates {
            let mut header = [0_u8; 96];
            let is_perl = File::open(&candidate)
                .and_then(|mut file| file.read(&mut header))
                .is_ok()
                && header.starts_with(b"#!")
                && String::from_utf8_lossy(&header).contains("perl");
            let (program, prefix) = if is_perl {
                (OsString::from("perl"), vec![candidate])
            } else {
                (candidate, Vec::new())
            };
            let reader = Self {
                program,
                prefix,
                config: config.clone(),
            };
            if reader
                .command()
                .arg("-ver")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                return Ok(reader);
            }
        }
        Err("the independent development ExifTool oracle is unavailable".into())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.prefix)
            .arg("-config")
            .arg(&self.config)
            .args(["-charset", "filename=UTF8"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
    }

    fn inspect(&self, path: &Path) -> Result<BTreeMap<String, Value>, String> {
        let output = preserving_access(path, || {
            self.command()
                .args([
                    "-j",
                    "-G1",
                    "-a",
                    "-n",
                    "-api",
                    "ImageHashType=SHA256",
                    "-FileType",
                    "-Make",
                    "-Model",
                    "-Software",
                    "-Orientation",
                    "-DateTimeOriginal",
                    "-SubSecTimeOriginal",
                    "-OffsetTimeOriginal",
                    "-CreateDate",
                    "-ModifyDate",
                    "-ExposureTime",
                    "-FNumber",
                    "-ISO",
                    "-ExposureProgram",
                    "-ExposureCompensation",
                    "-MeteringMode",
                    "-Flash",
                    "-FocalLength",
                    "-LensMake",
                    "-LensModel",
                    "-LensType",
                    "-SerialNumber",
                    "-LensSerialNumber",
                    "-WhiteBalance",
                    "-FocusMode",
                    "-ShutterCount",
                    "-OriginalRawFileName",
                    "-OriginalRawFileDigest",
                    "-RawImageDigest",
                    "-NewRawImageDigest",
                    "-ImageDataHash",
                    "-XMP-PhotoPinFix:all",
                    "-Error",
                ])
                .arg(path)
                .output()
                .map_err(|error| error.to_string())
        })?;
        if !output.status.success() {
            return Err(output_error(&output));
        }
        let records: Vec<BTreeMap<String, Value>> = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid ExifTool response: {error}"))?;
        let mut record = records
            .into_iter()
            .next()
            .ok_or("ExifTool returned no RAW record")?;
        record.remove("SourceFile");
        if let Some(error) = record
            .iter()
            .find_map(|(key, value)| key.ends_with(":Error").then_some(value))
        {
            return Err(format!("ExifTool could not read RAW: {error}"));
        }
        Ok(record)
    }
}

#[derive(Clone, Debug)]
struct Times {
    accessed: SystemTime,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Times {
    fn capture(path: &Path) -> Result<Self, String> {
        let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            accessed: metadata.accessed().map_err(|error| error.to_string())?,
            modified: metadata.modified().map_err(|error| error.to_string())?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    fn check(&self, path: &Path, stage: &str) -> Result<(), String> {
        let after = Self::capture(path)?;
        ensure(
            after.accessed == self.accessed,
            format!(
                "{stage}: atime changed: {:?} -> {:?}",
                self.accessed, after.accessed
            ),
        )?;
        ensure(
            after.modified == self.modified,
            format!(
                "{stage}: mtime changed: {:?} -> {:?}",
                self.modified, after.modified
            ),
        )?;
        ensure(
            after.created == self.created,
            format!(
                "{stage}: creation/birth time changed: {:?} -> {:?}",
                self.created, after.created
            ),
        )?;
        #[cfg(unix)]
        ensure(
            after.identity == self.identity,
            format!(
                "{stage}: original inode changed: {:?} -> {:?}",
                self.identity, after.identity
            ),
        )?;
        Ok(())
    }
}

fn attributes_handle(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new().access_mode(0x0080 | 0x0100).open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

fn preserving_access<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let before = Times::capture(path)?;
    let handle = attributes_handle(path).map_err(|error| error.to_string())?;
    let result = operation();
    handle
        .set_times(FileTimes::new().set_accessed(before.accessed))
        .map_err(|error| format!("cannot restore fixture atime: {error}"))?;
    before.check(path, "test's read-only inspection")?;
    result
}

fn set_old_times(path: &Path) -> Result<Times, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let accessed = SystemTime::UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_700);
    let modified = SystemTime::UNIX_EPOCH + Duration::new(1_500_000_000, 987_654_300);
    file.set_times(
        FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )
    .map_err(|error| error.to_string())?;
    drop(file);
    // Compare the actual stored precision exactly on every platform, without
    // adding a permissive timestamp tolerance for Windows or coarse filesystems.
    Times::capture(path)
}

fn fixture_format(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_uppercase();
    REQUIRED_FORMATS
        .contains(&extension.as_str())
        .then_some(extension)
}

fn list_fixtures(directory: &Path, strict: bool) -> Result<Vec<PathBuf>, String> {
    let mut fixtures = BTreeSet::new();
    let manifest = directory.join("manifest.json");
    if manifest.exists() {
        let bytes = fs::read(&manifest).map_err(|error| error.to_string())?;
        let manifest: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid RAW manifest: {error}"))?;
        let photos = manifest
            .get("photos")
            .and_then(Value::as_array)
            .ok_or("RAW manifest has no photos array")?;
        for photo in photos {
            let filename = photo
                .get("file")
                .and_then(Value::as_str)
                .ok_or("RAW manifest photo has no file")?;
            let relative = Path::new(filename);
            ensure(
                !relative.is_absolute()
                    && !relative
                        .components()
                        .any(|component| matches!(component, Component::ParentDir)),
                format!("RAW manifest path must remain inside demo/raw: {filename}"),
            )?;
            if fixture_format(relative).is_some() {
                let path = directory.join(relative);
                if !path.is_file() {
                    if strict {
                        return Err(format!(
                            "manifest RAW fixture is missing: {}",
                            path.display()
                        ));
                    }
                    eprintln!(
                        "Skipping optional RAW sample that has not been downloaded: {}",
                        path.display()
                    );
                    continue;
                }
                fixtures.insert(path);
            }
        }
    }
    if directory.is_dir() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.is_file() && fixture_format(&path).is_some() {
                fixtures.insert(path);
            }
        }
    }
    Ok(fixtures.into_iter().collect())
}

fn find_photo(tool: &MetadataTool, directory: &Path, path: &Path) -> Result<Photo, String> {
    let scan = tool.scan_directory(directory)?;
    let photo = scan
        .photos
        .into_iter()
        .find(|photo| photo.path == path)
        .ok_or("RAW missing from metadata scan")?;
    if let Some(error) = &photo.error {
        return Err(format!("RAW scan error: {error}"));
    }
    Ok(photo)
}

fn manifest_coordinates(record: &Value, key: &str) -> Result<Option<Coordinates>, String> {
    let value = record
        .get(key)
        .ok_or_else(|| format!("manifest lacks {key}"))?;
    if value.is_null() {
        return Ok(None);
    }
    let lat = value["lat"]
        .as_f64()
        .ok_or("manifest latitude is invalid")?;
    let lon = value["lon"]
        .as_f64()
        .ok_or("manifest longitude is invalid")?;
    Coordinates::new(lat, lon).map(Some)
}

fn matching_coordinates(actual: Option<Coordinates>, expected: Option<Coordinates>) -> bool {
    match (actual, expected) {
        (None, None) => true,
        (Some(actual), Some(expected)) => {
            let longitude = (actual.lon - expected.lon + 180.0).rem_euclid(360.0) - 180.0;
            (actual.lat - expected.lat).abs() <= 1e-7 && longitude.abs() <= 1e-7
        }
        _ => false,
    }
}

/// This runs in the same test as all RAW reads/writes so fixture atime guards
/// cannot race each other. JPEG anchors participate in exactly the same model.
fn verify_manifest_strategy(
    directory: &Path,
    tool: &MetadataTool,
    strict: bool,
) -> Result<BTreeMap<PathBuf, Candidate>, String> {
    let manifest_path = directory.join("manifest.json");
    if !manifest_path.is_file() {
        ensure(!strict, "strict RAW test requires manifest.json".into())?;
        return Ok(BTreeMap::new());
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(manifest_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let records = manifest["photos"]
        .as_array()
        .ok_or("manifest photos are missing")?;
    let mut expected = BTreeMap::new();
    let mut times = BTreeMap::new();
    for record in records {
        let name = record["file"]
            .as_str()
            .ok_or("manifest photo filename is missing")?;
        let relative = Path::new(name);
        ensure(
            !relative.is_absolute()
                && !relative
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)),
            format!("manifest filename leaves demo/raw: {name}"),
        )?;
        let path = directory.join(relative);
        times.insert(path.clone(), Times::capture(&path)?);
        ensure(
            expected.insert(path, record).is_none(),
            format!("duplicate manifest photo: {name}"),
        )?;
    }
    let mut photos = tool.scan_directory(directory)?.photos;
    ensure(
        photos.len() == expected.len(),
        "RAW manifest and scanned photo counts differ".into(),
    )?;
    sort_photos(&mut photos);
    recompute_candidates(&mut photos, Settings::default());
    let sessions = build_sessions(&photos);
    let mut counts = BTreeMap::from([
        ("photos", photos.len()),
        ("sessions", sessions.len()),
        ("green", 0_usize),
        ("yellow", 0),
        ("red", 0),
    ]);
    let mut candidates = BTreeMap::new();
    for (session_index, session) in sessions.iter().enumerate() {
        for &index in &session.indices {
            let photo = &photos[index];
            let record = expected
                .get(&photo.path)
                .ok_or_else(|| format!("unexpected photo {}", photo.path.display()))?;
            let name = record["file"].as_str().unwrap();
            ensure(
                photo.error.is_none(),
                format!("{name}: scan error {:?}", photo.error),
            )?;
            ensure(
                record["session"].as_u64() == Some(session_index as u64 + 1),
                format!("{name}: incorrect shooting session"),
            )?;
            let capture = chrono::NaiveDateTime::parse_from_str(
                record["captured_at"]
                    .as_str()
                    .ok_or("manifest capture time is missing")?,
                "%Y-%m-%dT%H:%M:%S%.f",
            )
            .map_err(|error| error.to_string())?;
            ensure(
                photo.captured_at == Some(capture),
                format!("{name}: capture time differs from manifest"),
            )?;
            let gps = manifest_coordinates(record, "gps")?;
            ensure(
                matching_coordinates(photo.gps, gps),
                format!("{name}: stored GPS differs from manifest"),
            )?;
            let candidate_coordinates = manifest_coordinates(record, "expected_coordinates")?;
            let expected_color = if gps.is_some() {
                "green"
            } else if candidate_coordinates.is_some() {
                "yellow"
            } else {
                "red"
            };
            let actual_color = if photo.gps.is_some() {
                "green"
            } else if photo.candidate.is_some() {
                "yellow"
            } else {
                "red"
            };
            ensure(
                actual_color == expected_color,
                format!("{name}: expected {expected_color}, got {actual_color}"),
            )?;
            *counts.get_mut(actual_color).unwrap() += 1;
            if gps.is_some() {
                ensure(
                    photo.candidate.is_none(),
                    format!("{name}: existing GPS received a candidate"),
                )?;
            } else {
                ensure(
                    matching_coordinates(
                        photo.candidate.map(|value| value.coordinates),
                        candidate_coordinates,
                    ),
                    format!("{name}: candidate coordinates differ from manifest"),
                )?;
                if let Some(candidate) = photo.candidate {
                    let method = record["expected_method"]
                        .as_str()
                        .and_then(Method::from_tag)
                        .ok_or("manifest candidate method is invalid")?;
                    ensure(
                        candidate.provenance
                            == Provenance {
                                mode: Mode::Auto,
                                method,
                            },
                        format!("{name}: candidate mode/method differs from manifest"),
                    )?;
                    candidates.insert(photo.path.clone(), candidate);
                }
            }
        }
    }
    for (name, actual) in &counts {
        ensure(
            manifest["counts"][*name].as_u64() == Some(*actual as u64),
            format!("RAW manifest {name} count differs: actual {actual}"),
        )?;
    }
    for (path, before) in times {
        before.check(&path, "manifest scan and strategy verification")?;
    }
    eprintln!(
        "PASS RAW manifest: {} sessions, {} green / {} yellow / {} red",
        counts["sessions"], counts["green"], counts["yellow"], counts["red"]
    );
    Ok(candidates)
}

fn write_strategy_candidate(
    source: &Path,
    tool: &MetadataTool,
    candidate: Candidate,
) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = directory
        .path()
        .join(source.file_name().ok_or("RAW filename is missing")?);
    preserving_access(source, || {
        fs::copy(source, &path)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })?;
    let times = set_old_times(&path)?;
    let outcome = tool.write_gps(&path, &candidate)?;
    ensure(
        outcome.warning.is_none(),
        format!("RAW strategy write cleanup warning: {:?}", outcome.warning),
    )?;
    let mut backup_name = path
        .file_name()
        .ok_or("RAW filename is missing")?
        .to_os_string();
    backup_name.push(".photopinfix-original");
    ensure(
        !path.with_file_name(backup_name).exists(),
        "default RAW write retained a backup".into(),
    )?;
    let written = find_photo(tool, directory.path(), &path)?;
    ensure(
        matching_coordinates(written.gps, Some(candidate.coordinates)),
        "strategy-generated RAW coordinates did not round-trip".into(),
    )?;
    ensure(
        written.provenance == Some(candidate.provenance),
        "strategy-generated RAW provenance did not round-trip".into(),
    )?;
    times.check(&path, "writing strategy-generated RAW candidate")
}

fn field<'a>(metadata: &'a BTreeMap<String, Value>, name: &str) -> Option<&'a Value> {
    metadata
        .iter()
        .find_map(|(key, value)| (key.rsplit(':').next() == Some(name)).then_some(value))
}

fn normalize_maker(value: &str) -> Option<&'static str> {
    let value = value.to_ascii_lowercase();
    REQUIRED_MAKERS
        .iter()
        .copied()
        .find(|maker| value.contains(maker))
        .or_else(|| value.contains("ricoh").then_some("pentax"))
        .or_else(|| value.contains("om digital").then_some("olympus"))
}

fn stable_metadata(metadata: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    metadata
        .iter()
        .filter(|(key, _)| !key.starts_with("XMP-PhotoPinFix:") && !key.ends_with(":ImageDataHash"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn same_file_bytes(first: &Path, second: &Path) -> Result<bool, String> {
    preserving_access(first, || {
        preserving_access(second, || {
            let mut first = File::open(first).map_err(|error| error.to_string())?;
            let mut second = File::open(second).map_err(|error| error.to_string())?;
            if first.metadata().map_err(|error| error.to_string())?.len()
                != second.metadata().map_err(|error| error.to_string())?.len()
            {
                return Ok(false);
            }
            let mut a = [0_u8; 64 * 1024];
            let mut b = [0_u8; 64 * 1024];
            loop {
                let read = first.read(&mut a).map_err(|error| error.to_string())?;
                if read == 0 {
                    return Ok(true);
                }
                second
                    .read_exact(&mut b[..read])
                    .map_err(|error| error.to_string())?;
                if a[..read] != b[..read] {
                    return Ok(false);
                }
            }
        })
    })
}

struct CaseResult {
    format: String,
    maker: String,
    wrote: bool,
    image_hash: bool,
}

fn exercise_fixture(
    source: &Path,
    tool: &MetadataTool,
    inspector: &ExifReader,
) -> Result<CaseResult, String> {
    let original_times = Times::capture(source)?;
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let filename = source.file_name().ok_or("RAW fixture has no filename")?;
    let path = directory.path().join(filename);
    preserving_access(source, || {
        fs::copy(source, &path)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })?;
    original_times.check(source, "copying demo fixture")?;
    let times = set_old_times(&path)?;
    let initial = find_photo(tool, directory.path(), &path)?;
    times.check(&path, "initial scan")?;
    ensure(
        initial.captured_at.is_some(),
        "RAW fixture has no genuine capture time".into(),
    )?;
    let before = inspector.inspect(&path)?;
    let make = field(&before, "Make")
        .and_then(Value::as_str)
        .ok_or("RAW fixture has no camera Make")?;
    let maker = normalize_maker(make)
        .ok_or_else(|| format!("unrecognized RAW maker: {make}"))?
        .to_owned();
    let model = field(&before, "Model")
        .and_then(Value::as_str)
        .ok_or("RAW fixture has no camera Model")?;
    ensure(
        !model.is_empty(),
        "RAW fixture camera Model is empty".into(),
    )?;
    let format = fixture_format(source).ok_or("unrecognized RAW extension")?;
    let actual_format = field(&before, "FileType")
        .and_then(Value::as_str)
        .ok_or("ExifTool did not identify RAW format")?;
    ensure(
        actual_format.eq_ignore_ascii_case(&format),
        format!("fixture .{format} is actually {actual_format}"),
    )?;
    let before_preview = load_preview(&path, tool, 640, 640)?.to_rgba8();
    ensure(
        before_preview.width() > 0 && before_preview.height() > 0,
        "RAW preview is empty".into(),
    )?;
    times.check(&path, "preview before GPS write")?;

    let candidate = Candidate {
        coordinates: Coordinates {
            lat: -33.8587333,
            lon: -151.2133444,
        },
        provenance: Provenance {
            mode: if matches!(format.as_str(), "ARW" | "CR3" | "RAF" | "ORF") {
                Mode::Auto
            } else {
                Mode::Manual
            },
            method: if matches!(format.as_str(), "ARW" | "CR3" | "RAF" | "ORF") {
                Method::TimeInterpolation
            } else {
                Method::CoordinateInput
            },
        },
    };
    if initial.gps.is_some() {
        ensure(
            tool.write_gps(&path, &candidate).is_err(),
            "existing RAW GPS was overwritten".into(),
        )?;
        times.check(&path, "refusing existing RAW GPS")?;
        ensure(
            same_file_bytes(&path, source)?,
            "existing-GPS RAW bytes changed".into(),
        )?;
        original_times.check(source, "existing-GPS fixture test")?;
        return Ok(CaseResult {
            format,
            maker,
            wrote: false,
            image_hash: field(&before, "ImageDataHash").is_some(),
        });
    }

    let outcome = tool.write_gps_with_backup(&path, &candidate, true)?;
    ensure(
        outcome.warning.is_none(),
        format!("RAW backup retention warning: {:?}", outcome.warning),
    )?;
    times.check(&path, "GPS write")?;
    let updated = find_photo(tool, directory.path(), &path)?;
    times.check(&path, "scan after GPS write")?;
    let gps = updated
        .gps
        .ok_or("RAW GPS was not readable after writing")?;
    ensure(
        (gps.lat - candidate.coordinates.lat).abs() <= 1e-7
            && (gps.lon - candidate.coordinates.lon).abs() <= 1e-7,
        format!(
            "negative RAW GPS coordinates changed: expected {:?}, actual {gps:?}",
            candidate.coordinates
        ),
    )?;
    ensure(
        updated.provenance == Some(candidate.provenance),
        format!(
            "PhotoPinFix Mode/Method did not round-trip: {:?}",
            updated.provenance
        ),
    )?;
    ensure(
        updated.captured_at == initial.captured_at,
        "RAW capture time changed".into(),
    )?;
    let after = inspector.inspect(&path)?;
    ensure(
        stable_metadata(&before) == stable_metadata(&after),
        format!(
            "camera/exposure/original-RAW metadata changed\nbefore: {:?}\nafter: {:?}",
            stable_metadata(&before),
            stable_metadata(&after)
        ),
    )?;
    let image_hash = if let Some(hash) = field(&before, "ImageDataHash") {
        ensure(
            hash.as_str().is_some_and(|value| !value.is_empty()),
            "original image-data hash is empty".into(),
        )?;
        ensure(
            field(&after, "ImageDataHash") == Some(hash),
            "RAW main image-data SHA256 changed".into(),
        )?;
        true
    } else {
        eprintln!(
            "{format}: ExifTool has no ImageDataHash for this sample; decoded preview pixels remain mandatory"
        );
        false
    };
    let custom_fields: BTreeSet<&str> = after
        .keys()
        .filter_map(|key| key.strip_prefix("XMP-PhotoPinFix:"))
        .collect();
    ensure(
        custom_fields == BTreeSet::from(["Mode", "Method"]),
        format!("unexpected PhotoPinFix fields: {custom_fields:?}"),
    )?;
    let after_preview = load_preview(&path, tool, 640, 640)?.to_rgba8();
    ensure(
        before_preview.dimensions() == after_preview.dimensions()
            && before_preview.as_raw() == after_preview.as_raw(),
        "RAW preview pixels changed after metadata write".into(),
    )?;
    times.check(&path, "all metadata and preview verification reads")?;
    let mut backup_name = filename.to_os_string();
    backup_name.push(".photopinfix-original");
    let backup = directory.path().join(backup_name);
    ensure(
        backup.is_file(),
        "original RAW backup was not created".into(),
    )?;
    ensure(
        same_file_bytes(&backup, source)?,
        "RAW backup does not match the original sample bytes".into(),
    )?;
    original_times.check(source, "complete RAW fixture test")?;
    times.check(&path, "complete RAW round-trip")?;
    Ok(CaseResult {
        format,
        maker,
        wrote: true,
        image_hash,
    })
}

fn ensure(condition: bool, message: String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message) }
}

fn output_error(output: &Output) -> String {
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if error.is_empty() {
        format!("ExifTool failed: {}", output.status)
    } else {
        error
    }
}

#[test]
fn real_camera_raw_preview_and_gps_round_trips() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let directory = root.join("demo/raw");
    let strict =
        std::env::var_os("PHOTOPINFIX_REQUIRE_RAW").is_some_and(|value| value == OsStr::new("1"));
    let fixtures =
        list_fixtures(&directory, strict).expect("real RAW fixture manifest must be valid");
    if fixtures.is_empty() && !strict {
        eprintln!(
            "Skipping real RAW round-trip: optional demo/raw samples are absent. Set PHOTOPINFIX_REQUIRE_RAW=1 to require them."
        );
        return;
    }
    assert!(
        !fixtures.is_empty(),
        "PHOTOPINFIX_REQUIRE_RAW=1 requires the demo/raw samples"
    );
    if strict {
        let formats: BTreeSet<String> = fixtures
            .iter()
            .filter_map(|path| fixture_format(path))
            .collect();
        let missing: Vec<&str> = REQUIRED_FORMATS
            .iter()
            .copied()
            .filter(|format| !formats.contains(*format))
            .collect();
        assert!(
            missing.is_empty(),
            "required RAW formats are missing: {missing:?}"
        );
    }
    let tool = MetadataTool::discover().expect("built-in metadata engine must be available");
    let inspector = match ExifReader::discover(&root) {
        Ok(inspector) => inspector,
        Err(error) => {
            assert!(
                !strict,
                "PHOTOPINFIX_REQUIRE_RAW=1 requires the independent development oracle: {error}"
            );
            eprintln!(
                "SKIP development RAW oracle checks: {error}. standalone_metadata still validates native metadata reads/writes without external programs."
            );
            return;
        }
    };
    let mut written_formats = BTreeSet::new();
    let mut written_makers = BTreeSet::new();
    let mut failures = Vec::new();
    let strategy_candidates = match verify_manifest_strategy(&directory, &tool, strict) {
        Ok(candidates) => candidates,
        Err(error) => {
            failures.push(format!("RAW manifest/strategy: {error}"));
            BTreeMap::new()
        }
    };
    let mut hash_count = 0;
    let mut existing_count = 0;
    for fixture in &fixtures {
        if let Some(candidate) = strategy_candidates.get(fixture)
            && let Err(error) = write_strategy_candidate(fixture, &tool, *candidate)
        {
            failures.push(format!("{} strategy write: {error}", fixture.display()));
        }
        match exercise_fixture(fixture, &tool, &inspector) {
            Ok(result) => {
                eprintln!(
                    "PASS {}: {} {}, {}, preview/time preservation verified",
                    fixture.display(),
                    result.maker,
                    result.format,
                    if result.wrote {
                        "GPS + provenance round-trip"
                    } else {
                        "existing GPS unchanged"
                    }
                );
                if result.wrote {
                    written_formats.insert(result.format);
                    written_makers.insert(result.maker);
                    hash_count += usize::from(result.image_hash);
                } else {
                    existing_count += 1;
                }
            }
            Err(error) => {
                eprintln!("FAIL {}: {error}", fixture.display());
                failures.push(format!("{}: {error}", fixture.display()));
            }
        }
    }
    if strict {
        for format in REQUIRED_FORMATS {
            if !written_formats.contains(*format) {
                failures.push(format!("no successful missing-GPS {format} round-trip"));
            }
        }
        for maker in REQUIRED_MAKERS {
            if !written_makers.contains(*maker) {
                failures.push(format!("no successful {maker} RAW round-trip"));
            }
        }
    }
    eprintln!(
        "RAW summary: {} formats / {} makers written; {hash_count} image-data hash checks; {existing_count} existing-GPS files unchanged",
        written_formats.len(),
        written_makers.len()
    );
    assert!(
        failures.is_empty(),
        "RAW regressions:\n{}",
        failures.join("\n")
    );
}
