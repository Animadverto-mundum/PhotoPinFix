//! The complete HEIC preview path must work with no Python, Perl or decoder
//! helper on PATH. Public sample files are copied with source atime restored.
use std::{
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use photopinfix::{metadata::MetadataTool, preview::load_preview};

#[derive(Debug, PartialEq, Eq)]
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

fn fixtures() -> Vec<PathBuf> {
    let mut photos =
        vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/heic/gradient.heic")];
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("demo/photos");
    let Ok(entries) = fs::read_dir(directory) else {
        return photos;
    };
    photos.extend(
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|value| value.eq_ignore_ascii_case("heic"))
            })
            .collect::<Vec<_>>(),
    );
    photos
}

#[test]
fn heic_previews_without_any_external_program_on_path() {
    assert!(
        fixtures()[0].is_file(),
        "bundled synthetic HEIC fixture is missing"
    );
    if std::env::var("PHOTOPINFIX_REQUIRE_DEMO").as_deref() == Ok("1") {
        assert!(
            fixtures().len() >= 6,
            "required five demo HEIC fixtures are absent"
        );
    }
    let outside_project = tempfile::tempdir().unwrap();
    let empty_path = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "heic_preview_child", "--nocapture"])
        .env("PHOTOPINFIX_HEIC_CHILD", "1")
        .env("PATH", empty_path.path())
        .env(
            "PHOTOPINFIX_PYTHON",
            empty_path.path().join("missing-python"),
        )
        .env(
            "PHOTOPINFIX_EXIFTOOL",
            empty_path.path().join("missing-exiftool"),
        )
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .current_dir(outside_project.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native HEIC child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn heic_preview_child() {
    if std::env::var("PHOTOPINFIX_HEIC_CHILD").as_deref() != Ok("1") {
        return;
    }
    let tool =
        MetadataTool::discover().expect("built-in metadata must not need an external executable");
    let directory = tempfile::tempdir().unwrap();
    let samples = fixtures();
    assert!(!samples.is_empty());
    for (index, source) in samples.iter().enumerate() {
        let before = Times::read(source);
        let path = directory
            .path()
            .join(format!("照片 with spaces {index}.heic"));
        fs::copy(source, &path).unwrap();
        attributes_handle(source)
            .set_times(FileTimes::new().set_accessed(before.accessed))
            .unwrap();
        assert_eq!(Times::read(source), before, "fixture times changed");
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
        let preview = load_preview(&path, &tool, 320, 240).unwrap();
        assert!(preview.width() > 0 && preview.width() <= 320);
        assert!(preview.height() > 0 && preview.height() <= 240);
        assert!(preview.to_rgb8().pixels().any(|pixel| pixel.0 != [0, 0, 0]));
        assert_eq!(
            Times::read(&path),
            times,
            "HEIC decode changed a file timestamp"
        );
    }
    let broken = directory.path().join("broken.heic");
    fs::write(&broken, b"not a HEIF image").unwrap();
    let times = Times::read(&broken);
    assert!(load_preview(&broken, &tool, 320, 240).is_err());
    assert_eq!(
        Times::read(&broken),
        times,
        "failed HEIC decode changed file times"
    );
    println!(
        "PASS {} native HEIC previews with empty PATH, Unicode/spaces, outside project cwd; success/error timestamps preserved",
        samples.len()
    );
}
