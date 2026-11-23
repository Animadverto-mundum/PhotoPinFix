/// Parse a latitude or longitude as signed decimal degrees or degrees/minutes/seconds.
/// A direction may appear before or after the number. Explicit signs must agree
/// with that direction, so `-12 N` is rejected instead of silently changing it.
pub fn parse_coordinate(text: &str, is_latitude: bool) -> Result<f64, String> {
    let normalized = text.trim().replace('−', "-").to_ascii_uppercase();
    if normalized.is_empty() {
        return Err("请输入坐标".into());
    }
    let mut number = normalized.as_str();
    let mut hemisphere = None;
    if number.starts_with(['N', 'S', 'E', 'W']) {
        hemisphere = number.chars().next();
        number = number[1..].trim();
    } else if number.ends_with(['N', 'S', 'E', 'W']) {
        hemisphere = number.chars().next_back();
        number = number[..number.len() - 1].trim();
    }
    let direction = match hemisphere {
        Some('N') if is_latitude => Some(1.0),
        Some('S') if is_latitude => Some(-1.0),
        Some('E') if !is_latitude => Some(1.0),
        Some('W') if !is_latitude => Some(-1.0),
        Some(_) => return Err("纬度使用 N/S，经度使用 E/W".into()),
        None => None,
    };

    let explicit_sign = if number.starts_with('-') {
        Some(-1.0)
    } else if number.starts_with('+') {
        Some(1.0)
    } else {
        None
    };
    if let (Some(sign), Some(direction)) = (explicit_sign, direction)
        && sign != direction
    {
        return Err("坐标正负号与方向不一致".into());
    }

    let mut numeric = String::new();
    for character in number.chars() {
        match character {
            '°' | 'º' | '\'' | '"' | '′' | '″' | ':' => numeric.push(' '),
            c if c.is_ascii_digit() || matches!(c, '.' | '-' | '+') => numeric.push(c),
            c if c.is_whitespace() => numeric.push(' '),
            _ => return Err("使用小数度或度分秒，例如 31.2304 或 31°13′49.44″N".into()),
        }
    }
    let pieces: Vec<&str> = numeric.split_whitespace().collect();
    if pieces.is_empty() || pieces.len() > 3 {
        return Err("坐标应包含度，或度和分，或度分秒".into());
    }
    let values: Vec<f64> = pieces
        .iter()
        .enumerate()
        .map(|(index, piece)| {
            if index > 0 && piece.contains(['-', '+']) {
                return Err("分和秒不能带正负号".to_string());
            }
            piece
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| "坐标包含无效数字".to_string())
        })
        .collect::<Result<_, _>>()?;
    if values.len() > 1 && values[0].fract() != 0.0 {
        return Err("使用度分秒时，度应为整数".into());
    }
    if values.len() > 2 && values[1].fract() != 0.0 {
        return Err("同时输入秒时，分应为整数".into());
    }
    for value in values.iter().skip(1) {
        if !(0.0..60.0).contains(value) {
            return Err("分和秒必须大于等于 0 且小于 60".into());
        }
    }

    let sign = direction.unwrap_or_else(|| {
        if values[0].is_sign_negative() {
            -1.0
        } else {
            1.0
        }
    });
    let degrees = values[0].abs()
        + values.get(1).copied().unwrap_or(0.0) / 60.0
        + values.get(2).copied().unwrap_or(0.0) / 3600.0;
    let limit = if is_latitude { 90.0 } else { 180.0 };
    if degrees > limit {
        return Err(format!("坐标不能超过 {limit} 度"));
    }
    Ok(sign * degrees)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoordinateFormat {
    #[default]
    Decimal,
    Dms,
}

impl CoordinateFormat {
    pub fn toggle(self) -> Self {
        match self {
            Self::Decimal => Self::Dms,
            Self::Dms => Self::Decimal,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Decimal => "小数度",
            Self::Dms => "度分秒",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoordinateNotation {
    #[default]
    Signed,
    Hemispheres,
}

impl CoordinateNotation {
    pub fn toggle(self) -> Self {
        match self {
            Self::Signed => Self::Hemispheres,
            Self::Hemispheres => Self::Signed,
        }
    }
}

pub fn format_coordinate(
    value: f64,
    latitude: bool,
    format: CoordinateFormat,
    notation: CoordinateNotation,
) -> String {
    let direction = match (latitude, value.is_sign_negative()) {
        (true, false) => 'N',
        (true, true) => 'S',
        (false, false) => 'E',
        (false, true) => 'W',
    };
    match (format, notation) {
        (CoordinateFormat::Decimal, CoordinateNotation::Signed) => format!("{value:+.7}"),
        (CoordinateFormat::Decimal, CoordinateNotation::Hemispheres) => {
            format!("{:.7}°{direction}", value.abs())
        }
        (CoordinateFormat::Dms, CoordinateNotation::Hemispheres) => format_dms(value, latitude),
        (CoordinateFormat::Dms, CoordinateNotation::Signed) => {
            let mut result = format_dms(value, latitude);
            result.pop();
            format!(
                "{}{result}",
                if value.is_sign_negative() { '-' } else { '+' }
            )
        }
    }
}

pub fn format_decimal(value: f64, _is_latitude: bool) -> String {
    format!("{value:.7}")
}

pub fn format_dms(value: f64, is_latitude: bool) -> String {
    // Round first, then split, so 59.9999 seconds is carried into the next minute.
    let millis = (value.abs() * 3_600_000.0).round() as u64;
    let degrees = millis / 3_600_000;
    let minutes = (millis / 60_000) % 60;
    let seconds = (millis % 60_000) as f64 / 1000.0;
    let direction = match (is_latitude, value.is_sign_negative()) {
        (true, false) => 'N',
        (true, true) => 'S',
        (false, false) => 'E',
        (false, true) => 'W',
    };
    format!("{degrees}°{minutes:02}′{seconds:06.3}″{direction}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_and_hemisphere_notation_round_trip_in_both_formats() {
        for format in [CoordinateFormat::Decimal, CoordinateFormat::Dms] {
            for notation in [CoordinateNotation::Signed, CoordinateNotation::Hemispheres] {
                for (value, latitude) in [
                    (31.2304, true),
                    (-33.5, true),
                    (121.4737, false),
                    (-70.25, false),
                    (-0.5, true),
                ] {
                    let text = format_coordinate(value, latitude, format, notation);
                    assert!(
                        (parse_coordinate(&text, latitude).unwrap() - value).abs() < 1e-6,
                        "{text}"
                    );
                }
            }
        }
        assert_eq!(parse_coordinate("S33 30 0", true).unwrap(), -33.5);
        assert_eq!(parse_coordinate("W70.25", false).unwrap(), -70.25);
        assert!(parse_coordinate("+33 S", true).is_err());
    }

    #[test]
    fn decimal_and_dms_represent_the_same_location() {
        let expected = 31.2304;
        for input in [
            "31.2304",
            "31°13′49.44″N",
            "N 31 13 49.44",
            "+31:13:49.44 N",
        ] {
            assert!((parse_coordinate(input, true).unwrap() - expected).abs() < 1e-9);
        }
        assert!((parse_coordinate("121°28'25.32\"W", false).unwrap() + 121.4737).abs() < 1e-9);
        assert!((parse_coordinate("-0 30 0", true).unwrap() + 0.5).abs() < 1e-9);
        assert_eq!(parse_coordinate("-12 S", true).unwrap(), -12.0);
    }

    #[test]
    fn invalid_coordinates_are_rejected() {
        for input in [
            "NaN", "inf", "-12N", "+12S", "91", "90 0 1", "12 60", "12 0 60", "12 -3", "12 E",
            "N12N", "12.5 30", "1 2 3 4", "", "1e309",
        ] {
            assert!(parse_coordinate(input, true).is_err(), "accepted {input}");
        }
        assert!(parse_coordinate("180 0 1", false).is_err());
        assert_eq!(parse_coordinate("180 W", false).unwrap(), -180.0);
    }

    #[test]
    fn formatted_dms_round_trips_and_carries_seconds() {
        for (value, is_latitude) in [
            (31.2304, true),
            (-121.4737, false),
            (-0.5, true),
            (180.0, false),
        ] {
            let formatted = format_dms(value, is_latitude);
            assert!((parse_coordinate(&formatted, is_latitude).unwrap() - value).abs() < 1e-6);
        }
        assert_eq!(format_dms(12.9999999999, true), "13°00′00.000″N");
    }
}
