use chrono::Duration;

use crate::model::{Candidate, Coordinates, Method, Mode, Photo, Provenance, Session, Settings};

/// The model remains ordered by capture time even if the UI chooses another view.
pub fn sort_photos(photos: &mut [Photo]) {
    photos.sort_by(|a, b| match (a.captured_at, b.captured_at) {
        (Some(left), Some(right)) => left.cmp(&right).then_with(|| a.path.cmp(&b.path)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.path.cmp(&b.path),
    });
}

/// Photos with no capture time form their own final session.
/// Call `sort_photos` before grouping or calculating candidates.
pub fn build_sessions(photos: &[Photo]) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    for (index, photo) in photos.iter().enumerate() {
        let needs_new = match (sessions.last().and_then(|s| s.end), photo.captured_at) {
            _ if sessions.is_empty() => true,
            (Some(previous), Some(now)) => now - previous > Duration::hours(1),
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        if needs_new {
            sessions.push(Session {
                indices: vec![index],
                start: photo.captured_at,
                end: photo.captured_at,
            });
        } else if let Some(session) = sessions.last_mut() {
            session.indices.push(index);
            session.end = photo.captured_at;
        }
    }
    sessions
}

/// Recalculate automatic proposals in two linear passes. A proposal never becomes
/// a reference: every chain remains rooted at a photo whose GPS is already stored.
pub fn recompute_candidates(photos: &mut [Photo], settings: Settings) {
    let Ok(settings) = settings.validate() else {
        for photo in photos {
            if photo.gps.is_none()
                && photo
                    .candidate
                    .is_some_and(|c| c.provenance.mode == Mode::Auto)
            {
                photo.candidate = None;
            }
        }
        return;
    };
    let gap = Duration::seconds(settings.adjacent_gap_seconds);
    let window = Duration::seconds(settings.reference_window_seconds);
    let count = photos.len();
    let mut previous = vec![None; count];
    let mut next = vec![None; count];
    let mut anchor = None;
    for index in 0..count {
        if index > 0 && !connected(&photos[index - 1], &photos[index], gap) {
            anchor = None;
        }
        if photos[index].gps.is_some() {
            // A green photo is always a boundary, including one without a time.
            anchor = photos[index].captured_at.map(|_| index);
        } else if let Some(source) = anchor
            && within_window(&photos[source], &photos[index], window)
        {
            previous[index] = Some(source);
        }
    }
    anchor = None;
    for index in (0..count).rev() {
        if index + 1 < count && !connected(&photos[index], &photos[index + 1], gap) {
            anchor = None;
        }
        if photos[index].gps.is_some() {
            anchor = photos[index].captured_at.map(|_| index);
        } else if let Some(source) = anchor
            && within_window(&photos[index], &photos[source], window)
        {
            next[index] = Some(source);
        }
    }

    for index in 0..count {
        if photos[index].gps.is_some()
            || photos[index]
                .candidate
                .is_some_and(|c| c.provenance.mode == Mode::Manual)
        {
            continue;
        }
        let candidate = match (previous[index], next[index]) {
            (Some(before), Some(after)) => {
                interpolate(photos, before, index, after)
                    .ok()
                    .map(|coordinates| {
                        make_candidate(coordinates, Mode::Auto, Method::TimeInterpolation)
                    })
            }
            (Some(before), None) => photos[before]
                .gps
                .map(|coordinates| make_candidate(coordinates, Mode::Auto, Method::CopyPrevious)),
            (None, Some(after)) => photos[after]
                .gps
                .map(|coordinates| make_candidate(coordinates, Mode::Auto, Method::CopyNext)),
            (None, None) => None,
        };
        photos[index].candidate = candidate;
    }
}

fn connected(before: &Photo, after: &Photo, gap: Duration) -> bool {
    match (before.captured_at, after.captured_at) {
        (Some(left), Some(right)) => {
            let elapsed = right - left;
            // Session boundaries remain boundaries even when users raise the gap.
            elapsed >= Duration::zero() && elapsed < gap && elapsed <= Duration::hours(1)
        }
        _ => false,
    }
}

fn within_window(before: &Photo, after: &Photo, window: Duration) -> bool {
    match (before.captured_at, after.captured_at) {
        (Some(left), Some(right)) => {
            let elapsed = right - left;
            elapsed >= Duration::zero() && elapsed <= window
        }
        _ => false,
    }
}

pub fn manual_candidate(
    photos: &[Photo],
    index: usize,
    method: Method,
) -> Result<Candidate, String> {
    let photo = photos.get(index).ok_or("未选中照片")?;
    if photo.gps.is_some() {
        return Err("这张照片已有 GPS，跳过补全".into());
    }
    let current_time = photo
        .captured_at
        .ok_or("照片缺少拍摄时间，请直接输入坐标")?;
    let previous = (0..index).rev().find(|&i| photos[i].gps.is_some());
    let next = (index + 1..photos.len()).find(|&i| photos[i].gps.is_some());
    let coordinates = match method {
        Method::CopyPrevious => previous
            .and_then(|i| photos[i].captured_at.and(photos[i].gps))
            .ok_or("没有可参考的前图")?,
        Method::CopyNext => next
            .and_then(|i| photos[i].captured_at.and(photos[i].gps))
            .ok_or("没有可参考的后图")?,
        Method::CopyNearest => {
            let previous_distance = previous.and_then(|i| {
                photos[i]
                    .captured_at
                    .map(|time| (i, (current_time - time).abs()))
            });
            let next_distance = next.and_then(|i| {
                photos[i]
                    .captured_at
                    .map(|time| (i, (time - current_time).abs()))
            });
            let source = match (previous_distance, next_distance) {
                // Deterministic tie: use the earlier photo.
                (Some((left, a)), Some((right, b))) => {
                    if a <= b {
                        left
                    } else {
                        right
                    }
                }
                (Some((left, _)), None) => left,
                (None, Some((right, _))) => right,
                (None, None) => return Err("没有带拍摄时间的 GPS 参考照片".into()),
            };
            photos[source].gps.ok_or("参考照片没有 GPS")?
        }
        Method::TimeInterpolation => {
            let before = previous.ok_or("时间插值需要前后两张 GPS 参考照片")?;
            let after = next.ok_or("时间插值需要前后两张 GPS 参考照片")?;
            interpolate(photos, before, index, after)?
        }
        Method::CoordinateInput => return Err("请通过坐标编辑框输入经纬度".into()),
    };
    Ok(make_candidate(coordinates, Mode::Manual, method))
}

fn make_candidate(coordinates: Coordinates, mode: Mode, method: Method) -> Candidate {
    Candidate {
        coordinates,
        provenance: Provenance { mode, method },
    }
}

fn duration_weight(duration: Duration) -> f64 {
    duration
        .num_nanoseconds()
        .map(|value| value as f64)
        .unwrap_or_else(|| duration.num_milliseconds() as f64 * 1_000_000.0)
}

fn interpolate(
    photos: &[Photo],
    before: usize,
    target: usize,
    after: usize,
) -> Result<Coordinates, String> {
    let left = &photos[before];
    let right = &photos[after];
    let a = left.gps.ok_or("前图没有 GPS")?;
    let b = right.gps.ok_or("后图没有 GPS")?;
    let start = left.captured_at.ok_or("前图缺少拍摄时间")?;
    let end = right.captured_at.ok_or("后图缺少拍摄时间")?;
    let time = photos[target].captured_at.ok_or("当前照片缺少拍摄时间")?;
    if time < start || time > end || end < start {
        return Err("拍摄时间顺序不适合插值".into());
    }
    if start == end {
        return if a == b {
            Ok(a)
        } else {
            Err("前后参考时间相同而位置不同，无法按时间插值".into())
        };
    }
    let weight = duration_weight(time - start) / duration_weight(end - start);
    let lat = a.lat + (b.lat - a.lat) * weight;
    // Interpolate the short way around the date line, rather than via Greenwich.
    let longitude_delta = (b.lon - a.lon + 180.0).rem_euclid(360.0) - 180.0;
    let lon = (a.lon + longitude_delta * weight + 180.0).rem_euclid(360.0) - 180.0;
    Coordinates::new(lat, lon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime};
    use std::path::PathBuf;

    fn start() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn photo(seconds: i64, gps: Option<Coordinates>) -> Photo {
        Photo {
            path: PathBuf::from(format!("photo-{seconds:05}.jpg")),
            captured_at: Some(start() + Duration::seconds(seconds)),
            capture_time_info: Default::default(),
            gps,
            provenance: None,
            candidate: None,
            error: None,
        }
    }

    fn a() -> Coordinates {
        Coordinates::new(10.0, 20.0).unwrap()
    }
    fn b() -> Coordinates {
        Coordinates::new(25.0, 35.0).unwrap()
    }

    #[test]
    fn fifteen_minute_chain_matches_the_agreed_example() {
        let mut photos: Vec<Photo> = (0..=30).map(|i| photo(i * 30, None)).collect();
        photos[0].gps = Some(a());
        photos[30].gps = Some(b());
        recompute_candidates(&mut photos, Settings::default());
        assert_eq!(photos[4].candidate.unwrap().coordinates, a()); // 10:02
        assert_eq!(
            photos[4].candidate.unwrap().provenance.method,
            Method::CopyPrevious
        );
        assert_eq!(
            photos[15].candidate.unwrap().coordinates,
            Coordinates::new(17.5, 27.5).unwrap()
        );
        assert_eq!(photos[26].candidate.unwrap().coordinates, b()); // 10:13
        assert!(photos[1..30].iter().all(|p| p.candidate.is_some()));
    }

    #[test]
    fn adjacent_gap_is_strict_and_total_window_is_inclusive() {
        let mut disconnected = vec![
            photo(0, Some(a())),
            photo(59, None),
            photo(119, None),
            photo(149, None),
        ];
        recompute_candidates(&mut disconnected, Settings::default());
        assert!(disconnected[1].candidate.is_some());
        assert!(disconnected[2].candidate.is_none());
        assert!(disconnected[3].candidate.is_none());
        let mut chain: Vec<Photo> = (0..=21).map(|i| photo(i * 30, None)).collect();
        chain[0].gps = Some(a());
        recompute_candidates(&mut chain, Settings::default());
        assert!(chain[20].candidate.is_some()); // 600 seconds
        assert!(chain[21].candidate.is_none()); // 630 seconds
        chain[21].captured_at = Some(start() + Duration::seconds(600) + Duration::milliseconds(1));
        recompute_candidates(&mut chain, Settings::default());
        assert!(chain[21].candidate.is_none());
    }

    #[test]
    fn green_boundaries_replace_old_references_and_candidates_never_reanchor() {
        let mut photos = vec![photo(0, Some(a())), photo(30, Some(b())), photo(60, None)];
        recompute_candidates(&mut photos, Settings::default());
        assert_eq!(photos[2].candidate.unwrap().coordinates, b());
        let mut chain: Vec<Photo> = (0..=30).map(|i| photo(i * 30, None)).collect();
        chain[0].gps = Some(a());
        recompute_candidates(&mut chain, Settings::default());
        recompute_candidates(&mut chain, Settings::default());
        assert!(chain[21..].iter().all(|p| p.candidate.is_none()));
    }

    #[test]
    fn settings_recompute_preserves_manual_proposals() {
        let mut photos = vec![photo(0, Some(a())), photo(30, None), photo(50, None)];
        photos[1].candidate = Some(make_candidate(b(), Mode::Manual, Method::CoordinateInput));
        recompute_candidates(&mut photos, Settings::default());
        assert_eq!(photos[1].candidate.unwrap().coordinates, b());
        assert_eq!(photos[2].candidate.unwrap().coordinates, a());
        recompute_candidates(
            &mut photos,
            Settings {
                adjacent_gap_seconds: 10,
                ..Settings::default()
            },
        );
        assert!(photos[2].candidate.is_none());
        assert_eq!(photos[1].candidate.unwrap().coordinates, b());
    }

    #[test]
    fn manual_shortcuts_ignore_automatic_thresholds() {
        let photos = vec![
            photo(0, Some(a())),
            photo(1200, None),
            photo(3600, Some(b())),
        ];
        let candidate = manual_candidate(&photos, 1, Method::TimeInterpolation).unwrap();
        assert_eq!(candidate.provenance.mode, Mode::Manual);
        assert_eq!(candidate.coordinates, Coordinates::new(15.0, 25.0).unwrap());
        assert_eq!(
            manual_candidate(&photos, 1, Method::CopyNearest)
                .unwrap()
                .coordinates,
            a()
        );
        assert!(manual_candidate(&photos, 0, Method::CopyNext).is_err());
    }

    #[test]
    fn interpolation_uses_short_longitude_arc_and_rejects_ambiguous_same_times() {
        let mut photos = vec![
            photo(0, Some(Coordinates::new(0.0, 179.0).unwrap())),
            photo(30, None),
            photo(60, Some(Coordinates::new(0.0, -179.0).unwrap())),
        ];
        recompute_candidates(&mut photos, Settings::default());
        assert_eq!(photos[1].candidate.unwrap().coordinates.lon.abs(), 180.0);
        for p in &mut photos {
            p.captured_at = Some(start());
        }
        recompute_candidates(&mut photos, Settings::default());
        assert!(photos[1].candidate.is_none());
        assert!(manual_candidate(&photos, 1, Method::TimeInterpolation).is_err());
    }

    #[test]
    fn sessions_split_only_above_one_hour_and_group_unknown_times_last() {
        let mut photos = vec![photo(7201, None), photo(3600, None), photo(0, None)];
        let mut unknown = photo(1, None);
        unknown.captured_at = None;
        photos.insert(0, unknown.clone());
        photos.push(unknown);
        sort_photos(&mut photos);
        let sessions = build_sessions(&photos);
        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions[0].indices.len(), 2); // Exactly one hour stays together.
        assert_eq!(sessions[1].indices.len(), 1);
        assert_eq!(sessions[2].indices.len(), 2);
        assert!(sessions[2].start.is_none());
    }
}
