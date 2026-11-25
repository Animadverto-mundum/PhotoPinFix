//! End-to-end checks against downloaded photographs with explicitly simulated
//! metadata. No fixture is modified: write tests use disposable copies.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::NaiveDateTime;
use photopinfix::{
    metadata::MetadataTool,
    model::{Candidate, Coordinates, Method, Mode, Photo, Provenance, Settings},
    preview::load_preview,
    strategy::{build_sessions, recompute_candidates, sort_photos},
};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Times {
    accessed: SystemTime,
    modified: SystemTime,
    created: Option<SystemTime>,
}

impl Times {
    fn read(path: &Path) -> Self {
        let metadata = fs::metadata(path).unwrap();
        Self {
            accessed: metadata.accessed().unwrap(),
            modified: metadata.modified().unwrap(),
            created: metadata.created().ok(),
        }
    }

    fn assert_unchanged(&self, path: &Path, operation: &str) {
        assert_eq!(
            &Self::read(path),
            self,
            "{operation} changed file timestamps: {}",
            path.display()
        );
    }
}

fn dataset() -> Option<(PathBuf, Value, MetadataTool)> {
    let root = std::env::var_os("PHOTOPINFIX_DEMO")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("demo"));
    let required = std::env::var("PHOTOPINFIX_REQUIRE_DEMO").as_deref() == Ok("1");
    let manifest_path = root.join("expected.json");
    if !manifest_path.is_file() || !root.join("photos").is_dir() {
        let message = format!(
            "Demo fixtures missing at {}. Generate them with python3 scripts/create_demo.py",
            root.display()
        );
        assert!(!required, "PHOTOPINFIX_REQUIRE_DEMO=1: {message}");
        eprintln!("SKIP demo integration: {message}");
        return None;
    }
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["simulated_metadata"], true);
    let tool = MetadataTool::discover().expect("built-in metadata engine must be available");
    Some((root, manifest, tool))
}

fn expected_coordinates(value: &Value) -> Option<Coordinates> {
    if value.is_null() {
        None
    } else {
        Some(
            Coordinates::new(
                value["lat"].as_f64().unwrap(),
                value["lon"].as_f64().unwrap(),
            )
            .unwrap(),
        )
    }
}

fn assert_coordinates(actual: Option<Coordinates>, expected: Option<Coordinates>, name: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => {
            let longitude_error = (actual.lon - expected.lon + 180.0).rem_euclid(360.0) - 180.0;
            assert!(
                (actual.lat - expected.lat).abs() <= 1e-7 && longitude_error.abs() <= 1e-7,
                "{name}: coordinates differ: actual {actual:?}, expected {expected:?}"
            );
        }
        _ => panic!("{name}: GPS presence differs: actual {actual:?}, expected {expected:?}"),
    }
}

fn basename(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn attributes_handle(path: &Path) -> File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // File::open only requests read access. SetFileTime additionally needs
        // FILE_WRITE_ATTRIBUTES, without requiring content write permission.
        fs::OpenOptions::new()
            .access_mode(0x0080 | 0x0100)
            .open(path)
            .unwrap()
    }
    #[cfg(not(windows))]
    {
        File::open(path).unwrap()
    }
}

fn read_preserving_access(path: &Path) -> Vec<u8> {
    let before = Times::read(path);
    let bytes = fs::read(path).unwrap();
    attributes_handle(path)
        .set_times(FileTimes::new().set_accessed(before.accessed))
        .unwrap();
    before.assert_unchanged(path, "test fixture content read");
    bytes
}

fn copy_preserving_source(source: &Path, destination: &Path) {
    let before = Times::read(source);
    fs::copy(source, destination).unwrap();
    attributes_handle(source)
        .set_times(FileTimes::new().set_accessed(before.accessed))
        .unwrap();
    before.assert_unchanged(source, "copying a test fixture");
}

fn write_and_verify_copy(
    tool: &MetadataTool,
    original: &Photo,
    directory: &Path,
    filename: &str,
    candidate: Candidate,
) {
    let destination = directory.join(filename);
    copy_preserving_source(&original.path, &destination);
    let fixed_time = UNIX_EPOCH + Duration::from_secs(1_577_934_245);
    attributes_handle(&destination)
        .set_times(
            FileTimes::new()
                .set_accessed(fixed_time)
                .set_modified(fixed_time),
        )
        .unwrap();
    let before = Times::read(&destination);
    let pixels_before = load_preview(&destination, tool, 320, 240)
        .unwrap_or_else(|error| panic!("{filename} preview before write: {error}"))
        .to_rgba8();
    before.assert_unchanged(&destination, "preview before GPS write");

    let outcome = tool
        .write_gps(&destination, &candidate)
        .unwrap_or_else(|error| panic!("{filename} real GPS write failed: {error}"));
    assert!(
        outcome.warning.is_none(),
        "{filename}: {:?}",
        outcome.warning
    );
    let mut backup_name = destination.file_name().unwrap().to_os_string();
    backup_name.push(".photopinfix-original");
    assert!(
        !destination.with_file_name(backup_name).exists(),
        "{filename}: default write retained a backup"
    );
    before.assert_unchanged(&destination, "GPS write");
    let rescanned = tool.scan_directory(directory).unwrap();
    let updated = rescanned
        .photos
        .iter()
        .find(|photo| basename(&photo.path) == filename)
        .unwrap();
    assert_coordinates(updated.gps, Some(candidate.coordinates), filename);
    assert_eq!(updated.provenance, Some(candidate.provenance), "{filename}");
    assert_eq!(updated.captured_at, original.captured_at, "{filename}");
    assert!(updated.error.is_none(), "{filename}: {:?}", updated.error);
    before.assert_unchanged(&destination, "reading back written metadata");

    let pixels_after = load_preview(&destination, tool, 320, 240)
        .unwrap_or_else(|error| panic!("{filename} preview after write: {error}"))
        .to_rgba8();
    assert_eq!(
        pixels_before, pixels_after,
        "{filename}: GPS write changed preview pixels"
    );
    before.assert_unchanged(&destination, "preview after GPS write");

    // Once a file has GPS it must be skipped, including files previously written
    // by PhotoPinFix. Verify the rejection leaves the entire file unchanged.
    let written = read_preserving_access(&destination);
    assert!(
        tool.write_gps(
            &destination,
            &Candidate {
                coordinates: Coordinates::new(-12.5, 87.25).unwrap(),
                provenance: Provenance {
                    mode: Mode::Manual,
                    method: Method::CoordinateInput,
                },
            },
        )
        .is_err(),
        "{filename}: an existing GPS location was overwritten"
    );
    assert_eq!(written, read_preserving_access(&destination), "{filename}");
    before.assert_unchanged(&destination, "refusing to overwrite existing GPS");
}

#[test]
fn fixture_time_handles_can_restore_access_without_changing_other_times() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("timestamp-check.bin");
    fs::write(&path, b"fixture timestamp permissions").unwrap();
    let created = Times::read(&path).created;
    // Darwin can move birthtime backwards when explicitly setting an older
    // mtime. Keep fixture setup after birthtime so this test isolates access
    // restoration instead of asserting a platform-specific setup side effect.
    let modified = SystemTime::now() + Duration::from_secs(60);
    attributes_handle(&path)
        .set_times(
            FileTimes::new()
                .set_accessed(UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_700))
                .set_modified(modified),
        )
        .unwrap();
    let before = Times::read(&path);
    assert_eq!(before.created, created);
    assert_eq!(
        read_preserving_access(&path),
        b"fixture timestamp permissions"
    );
    before.assert_unchanged(&path, "restoring access time with an attributes handle");
    copy_preserving_source(&path, &directory.path().join("copy.bin"));
    before.assert_unchanged(&path, "copying with access time restoration");
}

#[test]
fn downloaded_demo_scans_interpolates_previews_and_writes_without_changing_file_times() {
    let Some((root, manifest, tool)) = dataset() else {
        return;
    };
    let directory = root.join("photos");
    let records = manifest["photos"].as_array().unwrap();
    let expected: HashMap<&str, &Value> = records
        .iter()
        .map(|record| (record["file"].as_str().unwrap(), record))
        .collect();
    let initial_times: HashMap<PathBuf, Times> = expected
        .keys()
        .map(|filename| {
            let path = directory.join(filename);
            let times = Times::read(&path);
            (path, times)
        })
        .collect();
    let scan = tool.scan_directory(&directory).unwrap();
    assert_eq!(
        scan.photos.len(),
        manifest["total"].as_u64().unwrap() as usize
    );
    for (path, before) in &initial_times {
        before.assert_unchanged(path, "directory metadata scan");
    }
    let mut photos = scan.photos;
    sort_photos(&mut photos);
    for photo in &photos {
        let filename = basename(&photo.path);
        let record = expected[filename.as_str()];
        let captured = record["captured_at"]
            .as_str()
            .map(|value| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").unwrap());
        assert_eq!(
            photo.captured_at, captured,
            "{filename}: capture time differs"
        );
        assert_coordinates(photo.gps, expected_coordinates(&record["gps"]), &filename);
        let provenance = &record["provenance"];
        if provenance.is_null() {
            assert!(
                photo.provenance.is_none(),
                "{filename}: unexpected provenance"
            );
        } else {
            let actual = photo.provenance.expect("Provenance tags were not read");
            assert_eq!(actual.mode.as_str(), provenance["mode"].as_str().unwrap());
            assert_eq!(
                actual.method.as_str(),
                provenance["method"].as_str().unwrap()
            );
        }
        assert!(photo.error.is_none(), "{filename}: {:?}", photo.error);
    }

    let sessions = build_sessions(&photos);
    let expected_sessions = manifest["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), expected_sessions.len());
    for (session_index, session) in sessions.iter().enumerate() {
        let record = &expected_sessions[session_index];
        assert_eq!(
            session.indices.len(),
            record["count"].as_u64().unwrap() as usize
        );
        assert_eq!(
            session
                .start
                .map(|time| time.format("%Y-%m-%dT%H:%M:%S").to_string()),
            record["start"].as_str().map(str::to_owned)
        );
        assert_eq!(
            session
                .end
                .map(|time| time.format("%Y-%m-%dT%H:%M:%S").to_string()),
            record["end"].as_str().map(str::to_owned)
        );
        for &index in &session.indices {
            let filename = basename(&photos[index].path);
            assert_eq!(
                expected[filename.as_str()]["session"].as_u64().unwrap(),
                session_index as u64 + 1,
                "{filename}"
            );
        }
    }

    recompute_candidates(&mut photos, Settings::default());
    let mut counts: HashMap<&str, u64> = HashMap::new();
    for photo in &photos {
        let filename = basename(&photo.path);
        let record = expected[filename.as_str()];
        let status = if photo.gps.is_some() {
            "green"
        } else if photo.candidate.is_some() {
            "yellow"
        } else {
            "red"
        };
        assert_eq!(
            status,
            record["expected"]["status"].as_str().unwrap(),
            "{filename}"
        );
        *counts.entry(status).or_default() += 1;
        let expected_candidate = &record["expected"]["candidate"];
        if expected_candidate.is_null() {
            assert!(photo.candidate.is_none(), "{filename}");
        } else {
            let candidate = photo.candidate.expect("Expected an automatic candidate");
            assert_coordinates(
                Some(candidate.coordinates),
                expected_coordinates(&expected_candidate["coordinates"]),
                &filename,
            );
            assert_eq!(
                candidate.provenance.mode.as_str(),
                expected_candidate["mode"].as_str().unwrap(),
                "{filename}"
            );
            assert_eq!(
                candidate.provenance.method.as_str(),
                expected_candidate["method"].as_str().unwrap(),
                "{filename}"
            );
        }
    }
    for status in ["green", "yellow", "red"] {
        assert_eq!(
            counts.get(status).copied().unwrap_or_default(),
            manifest["counts"][status].as_u64().unwrap()
        );
    }

    let mut format_samples = BTreeMap::new();
    let mut write_samples = BTreeMap::new();
    for photo in &photos {
        let extension = photo
            .path
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        format_samples.entry(extension.clone()).or_insert(photo);
        if photo.gps.is_none() && photo.candidate.is_some() {
            write_samples.entry(extension).or_insert(photo);
        }
    }
    for extension in ["jpg", "png", "tif", "heic"] {
        assert!(
            format_samples.contains_key(extension),
            "Missing real {extension} fixture"
        );
        assert!(
            write_samples.contains_key(extension),
            "Missing writable {extension} fixture"
        );
    }
    for (format, photo) in format_samples {
        let before = Times::read(&photo.path);
        let image = load_preview(&photo.path, &tool, 640, 400)
            .unwrap_or_else(|error| panic!("{format}: preview failed: {error}"));
        assert!(image.width() > 0 && image.width() <= 640);
        assert!(image.height() > 0 && image.height() <= 400);
        before.assert_unchanged(&photo.path, &format!("{format} preview"));
    }

    let copies = tempfile::tempdir().unwrap();
    for (format, photo) in write_samples {
        write_and_verify_copy(
            &tool,
            photo,
            copies.path(),
            &format!("auto.{format}"),
            photo.candidate.unwrap(),
        );
    }
    let manual_source = photos
        .iter()
        .find(|photo| photo.gps.is_none() && photo.captured_at.is_none())
        .unwrap();
    let extension = manual_source.path.extension().unwrap().to_string_lossy();
    write_and_verify_copy(
        &tool,
        manual_source,
        copies.path(),
        &format!("manual.{extension}"),
        Candidate {
            coordinates: Coordinates::new(31.2304, -121.4737).unwrap(),
            provenance: Provenance {
                mode: Mode::Manual,
                method: Method::CoordinateInput,
            },
        },
    );
    for (path, before) in &initial_times {
        before.assert_unchanged(path, "complete demo integration test");
    }
}
