//! Dependency-free RFC 3339 ↔ epoch-milliseconds conversion for the
//! `Value::Instant` scalar (`#inst` literals).
//!
//! Kept in cljrs-types (no external deps) so the restricted `cljrs-tx`
//! profile's dependency tree does not grow; rich date-time operations belong
//! to `cljrs-builtins` (which may use full-featured crates).

/// Parse an RFC 3339 timestamp (`2026-08-21T12:34:56.789Z`,
/// `2026-08-21T12:34:56+02:00`, or the date-only `2026-08-21`) to epoch
/// milliseconds (UTC).
pub fn parse_rfc3339_millis(s: &str) -> Result<i64, String> {
    let bytes = s.as_bytes();
    let err = || format!("invalid #inst timestamp {s:?}");

    let digits = |range: std::ops::Range<usize>| -> Result<i64, String> {
        let part = bytes.get(range).ok_or_else(err)?;
        if part.is_empty() || !part.iter().all(u8::is_ascii_digit) {
            return Err(err());
        }
        Ok(std::str::from_utf8(part).unwrap().parse::<i64>().unwrap())
    };

    // Date part: YYYY-MM-DD
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(err());
    }
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(err());
    }

    let (mut hour, mut minute, mut second, mut millis) = (0i64, 0i64, 0i64, 0i64);
    let mut offset_minutes = 0i64;

    if bytes.len() > 10 {
        // Time part: THH:MM[:SS[.fff...]] then Z or ±HH:MM
        if !matches!(bytes[10], b'T' | b't' | b' ') || bytes.len() < 16 || bytes[13] != b':' {
            return Err(err());
        }
        hour = digits(11..13)?;
        minute = digits(14..16)?;
        let mut i = 16;
        if bytes.get(i) == Some(&b':') {
            second = digits(17..19).map_err(|_| err())?;
            i = 19;
        }
        if bytes.get(i) == Some(&b'.') {
            let start = i + 1;
            let mut end = start;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                return Err(err());
            }
            // Truncate/pad the fraction to milliseconds.
            let frac = &s[start..end];
            let ms_str = format!("{:0<3}", &frac[..frac.len().min(3)]);
            millis = ms_str.parse::<i64>().map_err(|_| err())?;
            i = end;
        }
        match bytes.get(i) {
            Some(b'Z') | Some(b'z') => {
                if i + 1 != bytes.len() {
                    return Err(err());
                }
            }
            Some(sign @ (b'+' | b'-')) => {
                if i + 6 != bytes.len() || bytes[i + 3] != b':' {
                    return Err(err());
                }
                let oh = digits(i + 1..i + 3)?;
                let om = digits(i + 4..i + 6)?;
                offset_minutes = oh * 60 + om;
                if *sign == b'-' {
                    offset_minutes = -offset_minutes;
                }
            }
            None => {} // bare local time: treat as UTC (Clojure would too for #inst without zone? it requires one; be lenient)
            _ => return Err(err()),
        }
        if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
            return Err(err());
        }
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    Ok(secs * 1_000 + millis)
}

/// Format epoch milliseconds as an RFC 3339 UTC timestamp with millisecond
/// precision: `2026-08-21T12:34:56.789-00:00` (Clojure prints `-00:00`).
pub fn format_rfc3339_millis(epoch_millis: i64) -> String {
    let millis = epoch_millis.rem_euclid(1_000);
    let total_secs = (epoch_millis - millis) / 1_000;
    let secs_of_day = total_secs.rem_euclid(86_400);
    let days = (total_secs - secs_of_day) / 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}-00:00"
    )
}

/// Days since 1970-01-01 from a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Civil date from days since 1970-01-01 (inverse of `days_from_civil`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_epoch() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(
            format_rfc3339_millis(0),
            "1970-01-01T00:00:00.000-00:00"
        );
    }

    #[test]
    fn parse_variants() {
        assert_eq!(
            parse_rfc3339_millis("2026-08-21T12:34:56.789Z").unwrap(),
            parse_rfc3339_millis("2026-08-21T14:34:56.789+02:00").unwrap(),
        );
        assert_eq!(
            parse_rfc3339_millis("2026-08-21").unwrap() % 86_400_000,
            0
        );
        // Fraction longer than millis truncates.
        assert_eq!(
            parse_rfc3339_millis("2026-08-21T00:00:00.123456Z").unwrap() % 1000,
            123
        );
        assert!(parse_rfc3339_millis("not-a-date").is_err());
        assert!(parse_rfc3339_millis("2026-13-01").is_err());
    }

    #[test]
    fn format_parse_roundtrip() {
        for millis in [0i64, 1_755_772_496_789, -86_400_000, 253_402_300_799_000] {
            let s = format_rfc3339_millis(millis);
            assert_eq!(parse_rfc3339_millis(&s).unwrap(), millis, "for {s}");
        }
    }

    #[test]
    fn negative_epoch_dates() {
        assert_eq!(
            format_rfc3339_millis(parse_rfc3339_millis("1969-12-31T23:59:59Z").unwrap()),
            "1969-12-31T23:59:59.000-00:00"
        );
    }
}
