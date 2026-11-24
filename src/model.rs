use std::path::PathBuf;

use chrono::{FixedOffset, NaiveDateTime, Timelike};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinates {
    pub lat: f64,
    pub lon: f64,
}

impl Coordinates {
    pub fn new(lat: f64, lon: f64) -> Result<Self, String> {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err("纬度必须在 -90 到 90 度之间".into());
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err("经度必须在 -180 到 180 度之间".into());
        }
        Ok(Self { lat, lon })
    }

    pub fn decimal(self) -> String {
        format!("{:.7}, {:.7}", self.lat, self.lon)
    }

    pub fn dms(self) -> String {
        format!(
            "{}  {}",
            crate::coordinates::format_dms(self.lat, true),
            crate::coordinates::format_dms(self.lon, false)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Manual,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    pub fn from_tag(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_tag(value).ok_or_else(|| format!("未知 GPS 来源模式：{value}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    TimeInterpolation,
    CopyPrevious,
    CopyNext,
    CopyNearest,
    CoordinateInput,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TimeInterpolation => "time-interpolation",
            Self::CopyPrevious => "copy-previous",
            Self::CopyNext => "copy-next",
            Self::CopyNearest => "copy-nearest",
            Self::CoordinateInput => "coordinate-input",
        }
    }

    pub fn from_tag(value: &str) -> Option<Self> {
        match value {
            "time-interpolation" => Some(Self::TimeInterpolation),
            "copy-previous" => Some(Self::CopyPrevious),
            "copy-next" => Some(Self::CopyNext),
            "copy-nearest" => Some(Self::CopyNearest),
            "coordinate-input" => Some(Self::CoordinateInput),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TimeInterpolation => "时间插值",
            Self::CopyPrevious => "采用前图",
            Self::CopyNext => "采用后图",
            Self::CopyNearest => "采用最近图",
            Self::CoordinateInput => "手填坐标",
        }
    }
}

impl std::str::FromStr for Method {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_tag(value).ok_or_else(|| format!("未知 GPS 补全方法：{value}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub mode: Mode,
    pub method: Method,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub coordinates: Coordinates,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Photo {
    pub path: PathBuf,
    /// Camera-local wall time, deliberately not converted to UTC. Existing
    /// inference rules compare this value, including cameras without an offset.
    pub captured_at: Option<NaiveDateTime>,
    pub capture_time_info: CaptureTimeInfo,
    pub gps: Option<Coordinates>,
    pub provenance: Option<Provenance>,
    pub candidate: Option<Candidate>,
    pub error: Option<String>,
}

/// Mutually exclusive workflow states. Persisted GPS always wins over a draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhotoStatus {
    NativeGps,
    Written,
    AutoPending,
    Unassigned,
    ManualPending,
}

impl PhotoStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NativeGps => "原有GPS",
            Self::Written => "已写入",
            Self::AutoPending => "自动待写",
            Self::Unassigned => "待指定",
            Self::ManualPending => "手动待写",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::NativeGps => "文件已有 GPS，且没有 PhotoPinFix 来源标记；不会补写",
            Self::Written => "文件已有 GPS，并带有 PhotoPinFix 来源标记；已成功写入",
            Self::AutoPending => "文件缺少 GPS，已有自动建议坐标，尚未写入",
            Self::Unassigned => "文件缺少 GPS，尚无可写入的坐标，需要手动指定",
            Self::ManualPending => "文件缺少 GPS，已手动指定坐标或参考方式，尚未写入",
        }
    }

    pub fn is_missing(self) -> bool {
        matches!(
            self,
            Self::AutoPending | Self::Unassigned | Self::ManualPending
        )
    }
}

/// Counts for one displayed scope. Previously patched files remain part of the
/// originally-missing total when their PhotoPinFix provenance is read again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhotoStats {
    pub total: usize,
    pub native_gps: usize,
    pub written: usize,
    pub auto_pending: usize,
    pub unassigned: usize,
    pub manual_pending: usize,
}

impl PhotoStats {
    pub fn from_photos<'a>(photos: impl IntoIterator<Item = &'a Photo>) -> Self {
        let mut stats = Self::default();
        for photo in photos {
            stats.total += 1;
            match photo.status() {
                PhotoStatus::NativeGps => stats.native_gps += 1,
                PhotoStatus::Written => stats.written += 1,
                PhotoStatus::AutoPending => stats.auto_pending += 1,
                PhotoStatus::Unassigned => stats.unassigned += 1,
                PhotoStatus::ManualPending => stats.manual_pending += 1,
            }
        }
        stats
    }

    pub fn missing_total(self) -> usize {
        self.written + self.auto_pending + self.unassigned + self.manual_pending
    }

    /// Coordinates assigned, including those already persisted successfully.
    pub fn assigned(self) -> usize {
        self.pending_write() + self.written
    }

    pub fn pending_write(self) -> usize {
        self.auto_pending + self.manual_pending
    }
}

/// Metadata needed to display the camera's timestamp without inventing a zone
/// or losing trailing zeroes / precision beyond Chrono's nanosecond storage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureTimeInfo {
    pub offset: Option<FixedOffset>,
    pub subsecond_digits: Option<String>,
}

impl Photo {
    pub fn status(&self) -> PhotoStatus {
        if self.gps.is_some() {
            if self.provenance.is_some() {
                PhotoStatus::Written
            } else {
                PhotoStatus::NativeGps
            }
        } else {
            match self.candidate.map(|candidate| candidate.provenance.mode) {
                Some(Mode::Auto) => PhotoStatus::AutoPending,
                Some(Mode::Manual) => PhotoStatus::ManualPending,
                None => PhotoStatus::Unassigned,
            }
        }
    }

    pub fn effective_coordinates(&self) -> Option<Coordinates> {
        self.gps.or_else(|| self.candidate.map(|c| c.coordinates))
    }

    pub fn capture_time_label(&self) -> String {
        let Some(time) = self.captured_at else {
            return "拍摄时间未知".into();
        };
        let mut label = time.format("%Y-%m-%d %H:%M:%S").to_string();
        if let Some(digits) = &self.capture_time_info.subsecond_digits {
            label.push('.');
            label.push_str(digits);
        } else if time.nanosecond() != 0 {
            // Useful for programmatically created photos without raw metadata.
            label.push('.');
            label.push_str(format!("{:09}", time.nanosecond()).trim_end_matches('0'));
        }
        label.push(' ');
        match self.capture_time_info.offset {
            Some(offset) => label.push_str(&offset.to_string()),
            None => label.push_str("[时区未知]"),
        }
        label
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub adjacent_gap_seconds: i64,
    pub reference_window_seconds: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            adjacent_gap_seconds: 60,
            reference_window_seconds: 600,
        }
    }
}

impl Settings {
    pub fn validate(self) -> Result<Self, String> {
        if self.adjacent_gap_seconds <= 0 || self.reference_window_seconds <= 0 {
            return Err("两个时间阈值都必须大于 0 秒".into());
        }
        if chrono::Duration::try_seconds(self.adjacent_gap_seconds).is_none()
            || chrono::Duration::try_seconds(self.reference_window_seconds).is_none()
        {
            return Err("时间阈值过大".into());
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub indices: Vec<usize>,
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
}

#[cfg(test)]
mod status_tests {
    use super::*;

    fn photo() -> Photo {
        Photo {
            path: "photo.jpg".into(),
            captured_at: None,
            capture_time_info: Default::default(),
            gps: None,
            provenance: None,
            candidate: None,
            error: None,
        }
    }

    #[test]
    fn five_states_count_once_and_persisted_gps_takes_precedence() {
        let coordinates = Coordinates::new(1.0, 2.0).unwrap();
        let auto = Provenance {
            mode: Mode::Auto,
            method: Method::CopyPrevious,
        };
        let manual = Provenance {
            mode: Mode::Manual,
            method: Method::CoordinateInput,
        };
        let mut photos = vec![photo(); 5];
        photos[0].gps = Some(coordinates);
        // Defensive: an obsolete draft must never turn an existing GPS into a gap.
        photos[0].candidate = Some(Candidate {
            coordinates,
            provenance: auto,
        });
        photos[1].gps = Some(coordinates);
        photos[1].provenance = Some(auto);
        photos[2].candidate = Some(Candidate {
            coordinates,
            provenance: auto,
        });
        photos[3].candidate = Some(Candidate {
            coordinates,
            provenance: manual,
        });
        photos[3].error = Some("write failed".into());
        photos[4].provenance = Some(manual); // A source tag alone is not written GPS.
        assert_eq!(
            photos.iter().map(Photo::status).collect::<Vec<_>>(),
            vec![
                PhotoStatus::NativeGps,
                PhotoStatus::Written,
                PhotoStatus::AutoPending,
                PhotoStatus::ManualPending,
                PhotoStatus::Unassigned,
            ]
        );
        let before = PhotoStats::from_photos(&photos);
        assert_eq!(
            before,
            PhotoStats {
                total: 5,
                native_gps: 1,
                written: 1,
                auto_pending: 1,
                unassigned: 1,
                manual_pending: 1
            }
        );
        assert_eq!(
            (
                before.missing_total(),
                before.assigned(),
                before.pending_write()
            ),
            (4, 3, 2)
        );
        photos[3].gps = Some(coordinates);
        photos[3].provenance = Some(manual);
        photos[3].candidate = None;
        let after = PhotoStats::from_photos(&photos);
        assert_eq!(after.written, 2);
        assert_eq!(after.manual_pending, 0);
        assert_eq!(after.missing_total(), before.missing_total());
        assert_eq!(after.assigned(), before.assigned());
        assert!(!photos[3].status().is_missing());
    }
}
