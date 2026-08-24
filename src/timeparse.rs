//! Minimal RFC3339-ish timestamp parsing without a date crate. Provider logs
//! write `2026-08-24T08:15:30.123Z` (Claude) or the same with an offset; we
//! only ever need epoch seconds for window math.

/// Parse `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]` to epoch seconds. Returns
/// None on anything that doesn't look like a timestamp — callers treat that
/// as "no evidence", never an error.
pub fn parse_epoch_secs(ts: &str) -> Option<i64> {
    let bytes = ts.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        ts.get(range)?.parse::<i64>().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut epoch = days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + s;
    // Skip fractional seconds, then apply an explicit offset if present.
    let mut rest = &ts[19..];
    if rest.starts_with('.') {
        let end = rest[1..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        rest = &rest[end..];
    }
    if let Some(offset) = rest.strip_prefix('+').map(|o| (o, -1)).or_else(|| {
        rest.strip_prefix('-').map(|o| (o, 1))
    }) {
        let (o, sign) = offset;
        if o.len() >= 5 {
            let oh: i64 = o.get(0..2)?.parse().ok()?;
            let om: i64 = o.get(3..5)?.parse().ok()?;
            epoch += sign * (oh * 3_600 + om * 60);
        }
    }
    Some(epoch)
}

/// Howard Hinnant's days-from-civil: civil date → days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "6d 2h" / "1h 20m" / "12m" / "now" — compact remaining-time text.
pub fn compact_duration(mut secs: i64) -> String {
    if secs <= 0 {
        return "now".into();
    }
    secs += 59; // round up to the minute the user will actually experience
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zulu_and_offset() {
        assert_eq!(parse_epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_epoch_secs("2026-08-24T09:15:05.709Z"), Some(1_787_562_905));
        assert_eq!(
            parse_epoch_secs("2026-08-24T11:15:05+02:00"),
            parse_epoch_secs("2026-08-24T09:15:05Z")
        );
        assert_eq!(parse_epoch_secs("not a time"), None);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(compact_duration(0), "now");
        assert_eq!(compact_duration(90), "2m");
        assert_eq!(compact_duration(4_800), "1h 20m");
        assert_eq!(compact_duration(530_000, ), "6d 3h");
    }
}
