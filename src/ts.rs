//! Timestamps to integer milliseconds since the Unix epoch (UTC). Strict ASCII digits.
//! Forms: F1 `2026-10-03T23:58:10Z`, F2 `2026-10-03T19:58:10-04:00`, F3 `1791071890000`.
//! `None` means a bad timestamp. An F3 number too long for the window (more than 18
//! significant digits) is also `None`: the caller drops it either way.

pub const MS_DAY: i64 = 86_400_000;
pub const WEEK_MS: i64 = 7 * MS_DAY;
pub const SESSION_GAP_MS: i64 = 1_800_000;
pub const LATE_MS: i64 = MS_DAY;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[inline]
fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[inline]
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
    }
}

#[inline]
fn num2(b: &[u8], i: usize) -> Option<i64> {
    let (x, y) = (b[i].wrapping_sub(b'0'), b[i + 1].wrapping_sub(b'0'));
    if x < 10 && y < 10 {
        Some((x * 10 + y) as i64)
    } else {
        None
    }
}

/// ASCII digits only, at least one. Leading zeros are fine. `None` if not all digits or
/// more than 18 significant digits.
#[inline]
pub fn digits_i64(b: &[u8]) -> Option<i64> {
    if b.is_empty() || !b.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let first = b.iter().position(|&c| c != b'0').unwrap_or(b.len());
    let s = &b[first..];
    if s.len() > 18 {
        return None;
    }
    let mut v: i64 = 0;
    for &c in s {
        v = v * 10 + (c - b'0') as i64;
    }
    Some(v)
}

pub fn parse_ts(b: &[u8]) -> Option<i64> {
    let n = b.len();
    if (n == 20 || n == 25) && b[4] == b'-' {
        return parse_iso(b);
    }
    digits_i64(b)
}

fn parse_iso(b: &[u8]) -> Option<i64> {
    let n = b.len();
    if b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let y = num2(b, 0)? * 100 + num2(b, 2)?;
    let mo = num2(b, 5)?;
    let d = num2(b, 8)?;
    let h = num2(b, 11)?;
    let mi = num2(b, 14)?;
    let s = num2(b, 17)?;
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let off = if n == 20 {
        if b[19] != b'Z' {
            return None;
        }
        0
    } else {
        let sign = match b[19] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if b[22] != b':' {
            return None;
        }
        let oh = num2(b, 20)?;
        let om = num2(b, 23)?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3_600_000 + om * 60_000)
    };
    Some(days_from_civil(y, mo, d) * MS_DAY + h * 3_600_000 + mi * 60_000 + s * 1000 - off)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        let want = 1_791_071_890_000;
        assert_eq!(parse_ts(b"2026-10-03T23:58:10Z"), Some(want));
        assert_eq!(parse_ts(b"2026-10-03T19:58:10-04:00"), Some(want));
        assert_eq!(parse_ts(b"1791071890000"), Some(want));
        assert_eq!(parse_ts(b"0001791071890000"), Some(want));
        assert_eq!(parse_ts(b"2026-10-03T23:58:10-00:00"), Some(want));
    }

    #[test]
    fn bad() {
        for s in [
            "", "2026-10-03T23:58:10", "2026-10-03t23:58:10Z", "2026-10-03 23:58:10Z", "2026-10-03T23:58:10z",
            "2026-02-30T00:00:00Z", "2027-02-29T00:00:00Z", "2026-13-01T00:00:00Z", "2026-10-03T24:00:00Z",
            "2026-10-03T23:58:60Z", "2026-10-03T00:00:00+24:00", "2026-10-03T00:00:00+05:60",
            "2026-10-03T00:00:00+0200", "1791071890000.0", "1.79e12", "-5", "٢٥٠",
        ] {
            assert_eq!(parse_ts(s.as_bytes()), None, "{s}");
        }
        assert!(parse_ts(b"2028-02-29T00:00:00Z").is_some());
        assert!(parse_ts("2026-10-03T00:00:00+23:59".as_bytes()).is_some());
    }

    /// Cross-check against the DuckDB reference's ts_ms(): scripts/ts_crosscheck.py writes the
    /// cases (`input<TAB>expected`, expected empty for bad) and sets TS_CASES to the file.
    #[test]
    fn against_reference() {
        let Ok(path) = std::env::var("TS_CASES") else { return };
        let text = std::fs::read_to_string(path).unwrap();
        let mut n = 0;
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            let (input, want) = line.split_once('\t').unwrap();
            let want: Option<i64> = if want.is_empty() { None } else { Some(want.parse().unwrap()) };
            assert_eq!(parse_ts(input.as_bytes()), want, "input {input:?}");
            n += 1;
        }
        eprintln!("checked {n} timestamp cases against the reference");
    }
}
