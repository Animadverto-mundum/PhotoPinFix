//! Exercise the actual application API with no external programs available.
//! JPEG/PNG and the bundled HEIC always run; downloaded RAW samples extend this
//! to twelve formats. raw_roundtrip.rs/demo_cases.rs supply the separate,
//! development-only ExifTool oracle checks without duplicating their machinery.
use std::{
    fs::{self, File, FileTimes},
    path::Path,
    process::Command,
    time::{Duration, SystemTime},
};

use photopinfix::{
    metadata::MetadataTool,
    model::{Candidate, Coordinates, Method, Mode, Photo, Provenance},
    preview::load_preview,
};

const RAW_SAMPLES: &[&str] = &[
    "sony_ilce7s.ARW",
    "canon_40d.CR2",
    "canon_r6.CR3",
    "leica_m8.DNG",
    "nikon_d1x.NEF",
    "fujifilm_s5000.RAF",
    "panasonic_lx7.RW2",
    "olympus_e3.ORF",
    "pentax_k10d.PEF",
];

#[derive(Debug, PartialEq, Eq)]
struct Times {
    accessed: SystemTime,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Times {
    fn read(path: &Path) -> Self {
        let metadata = fs::metadata(path).unwrap();
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            accessed: metadata.accessed().unwrap(),
            modified: metadata.modified().unwrap(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
        }
    }

    fn restore_access(&self, path: &Path) {
        attributes_handle(path)
            .set_times(FileTimes::new().set_accessed(self.accessed))
            .unwrap();
        assert_eq!(
            Self::read(path),
            *self,
            "file times/identity changed: {}",
            path.display()
        );
    }
}

fn attributes_handle(path: &Path) -> File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
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

fn copy_preserving_source(source: &Path, target: &Path) {
    let original = Times::read(source);
    let result = fs::copy(source, target);
    original.restore_access(source);
    result.unwrap();
}

fn read_preserving_access(path: &Path) -> Vec<u8> {
    let before = Times::read(path);
    let result = fs::read(path);
    before.restore_access(path);
    result.unwrap()
}

#[test]
fn metadata_round_trips_without_any_external_program_on_path() {
    let outside_project = tempfile::tempdir().unwrap();
    let empty_path = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "standalone_metadata_child", "--nocapture"])
        .env("PHOTOPINFIX_METADATA_CHILD", "1")
        .env("PATH", empty_path.path())
        .env(
            "PHOTOPINFIX_EXIFTOOL",
            empty_path.path().join("missing-exiftool"),
        )
        .env(
            "PHOTOPINFIX_PYTHON",
            empty_path.path().join("missing-python"),
        )
        .env_remove("PERL5LIB")
        .env_remove("PERL5OPT")
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .current_dir(outside_project.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "standalone metadata child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn standalone_metadata_child() {
    if std::env::var("PHOTOPINFIX_METADATA_CHILD").as_deref() != Ok("1") {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("中文 照片目录");
    fs::create_dir(&directory).unwrap();
    let image = image::RgbImage::from_fn(12, 8, |x, y| {
        image::Rgb([(x * 20) as u8, (y * 30) as u8, ((x + y) * 10) as u8])
    });
    let mut samples = Vec::new();
    for extension in ["jpg", "png"] {
        let source = directory.join(format!("合成 source.{extension}"));
        image.save(&source).unwrap();
        samples.push(source);
    }
    let heic = root.join("tests/fixtures/heic/gradient.heic");
    assert!(heic.is_file(), "bundled HEIC fixture missing");
    samples.push(heic);
    let require_raw = std::env::var("PHOTOPINFIX_REQUIRE_RAW").as_deref() == Ok("1");
    for filename in RAW_SAMPLES {
        let source = root.join("demo/raw-sources").join(filename);
        if source.is_file() {
            samples.push(source);
        } else {
            assert!(
                !require_raw,
                "required RAW fixture absent: {}",
                source.display()
            );
            println!("SKIP optional RAW fixture {filename}");
        }
    }
    let tool =
        MetadataTool::discover().expect("metadata engine must be built into the application");
    for (index, source) in samples.iter().enumerate() {
        let extension = source.extension().unwrap().to_string_lossy();
        let case_directory = directory.join(format!("case {index:02}"));
        fs::create_dir(&case_directory).unwrap();
        let path = case_directory.join(format!("照片 with spaces.{extension}"));
        copy_preserving_source(source, &path);
        let original_bytes = fs::read(&path).unwrap();
        attributes_handle(&path)
            .set_times(
                FileTimes::new()
                    .set_accessed(
                        SystemTime::UNIX_EPOCH + Duration::new(1_400_000_000, 123_456_700),
                    )
                    .set_modified(
                        SystemTime::UNIX_EPOCH + Duration::new(1_500_000_000, 987_654_300),
                    ),
            )
            .unwrap();
        let times = Times::read(&path);
        let initial = scan_one(&tool, &path);
        assert!(
            initial.gps.is_none(),
            "standalone write fixture must lack GPS: {}",
            source.display()
        );
        let preview_before = load_preview(&path, &tool, 240, 160).unwrap().to_rgba8();
        assert!(preview_before.width() > 0 && preview_before.height() > 0);
        assert_eq!(
            Times::read(&path),
            times,
            "{extension}: initial scan/preview altered times"
        );
        let candidate = Candidate {
            coordinates: Coordinates::new(-35.1234567, -120.7654321).unwrap(),
            provenance: if index % 2 == 0 {
                Provenance {
                    mode: Mode::Manual,
                    method: Method::CoordinateInput,
                }
            } else {
                Provenance {
                    mode: Mode::Auto,
                    method: Method::TimeInterpolation,
                }
            },
        };
        tool.write_gps(&path, &candidate).unwrap();
        assert_eq!(
            Times::read(&path),
            times,
            "{extension}: write altered times/identity"
        );
        let updated = scan_one(&tool, &path);
        let actual = updated.gps.expect("written GPS was not readable");
        assert!((actual.lat - candidate.coordinates.lat).abs() < 1e-7);
        assert!((actual.lon - candidate.coordinates.lon).abs() < 1e-7);
        assert_eq!(
            updated.provenance,
            Some(candidate.provenance),
            "{extension}: custom XMP did not round-trip"
        );
        assert_eq!(updated.captured_at, initial.captured_at);
        assert_eq!(updated.capture_time_info, initial.capture_time_info);
        assert_eq!(updated.capture_time_label(), initial.capture_time_label());
        let preview_after = load_preview(&path, &tool, 240, 160).unwrap().to_rgba8();
        assert_eq!(
            preview_after, preview_before,
            "{extension}: image preview pixels changed"
        );
        assert_eq!(
            Times::read(&path),
            times,
            "{extension}: verification reads altered times"
        );
        let backup =
            path.with_file_name(format!("照片 with spaces.{extension}.photopinfix-original"));
        assert_eq!(
            fs::read(backup).unwrap(),
            original_bytes,
            "{extension}: original backup differs"
        );
        let written_bytes = read_preserving_access(&path);
        assert!(
            tool.write_gps(&path, &candidate).is_err(),
            "{extension}: existing GPS was overwritten"
        );
        assert_eq!(read_preserving_access(&path), written_bytes);
        assert_eq!(Times::read(&path), times);
        println!(
            "PASS {extension}: scan, preview, GPS/custom XMP, original backup, timestamps/identity, existing-GPS refusal; empty PATH and Chinese/spaces"
        );
    }
    println!(
        "PASS {} standalone metadata formats; no external program was available on PATH",
        samples.len()
    );
}

fn scan_one(tool: &MetadataTool, path: &Path) -> Photo {
    let scan = tool.scan_directory(path.parent().unwrap()).unwrap();
    assert_eq!(
        scan.photos.len(),
        1,
        "backup must not appear as a second photo"
    );
    let photo = scan.photos.into_iter().next().unwrap();
    assert_eq!(photo.path, path);
    assert!(
        photo.error.is_none(),
        "metadata read failed: {:?}",
        photo.error
    );
    photo
}
