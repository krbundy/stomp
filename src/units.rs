//! Parsing and formatting of sizes, counts, durations and ages.

use std::time::Duration;

use anyhow::{Context, Result, bail};

const IEC_UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// Formats a byte count with binary units, e.g. `12.4 TiB`.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < IEC_UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    // Avoid printing "1024.0 GiB" when rounding pushes a value up to the next unit.
    if value >= 1023.95 && unit < IEC_UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", IEC_UNITS[unit])
}

/// Formats a count with thousands separators, e.g. `38,112,004`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Parses a size such as `50GiB`, `1.5T` or `500MB`.
///
/// `K`, `M`, `G`, `T`, `P` and the `iB` forms are binary (powers of 1024);
/// `KB`, `MB`, `GB`, `TB` and `PB` are decimal (powers of 1000).
pub fn parse_size(s: &str) -> Result<u64> {
    let t = s.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (number, unit) = t.split_at(split);
    if number.is_empty() {
        bail!("invalid size {s:?}: expected a number such as \"50GiB\"");
    }
    let value: f64 = number
        .parse()
        .with_context(|| format!("invalid size {s:?}"))?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kib" => 1024f64,
        "m" | "mib" => 1024f64.powi(2),
        "g" | "gib" => 1024f64.powi(3),
        "t" | "tib" => 1024f64.powi(4),
        "p" | "pib" => 1024f64.powi(5),
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        other => bail!("invalid size {s:?}: unknown unit {other:?}"),
    };
    let bytes = value * multiplier;
    if !bytes.is_finite() || bytes > u64::MAX as f64 {
        bail!("size {s:?} is too large");
    }
    Ok(bytes.round() as u64)
}

/// Parses a duration such as `30d`, `1h`, `2w` or `1y` (365 days).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (number, unit) = t.split_at(split);
    let n: u64 = number
        .parse()
        .with_context(|| format!("invalid duration {s:?}: expected a value such as \"30d\""))?;
    let seconds_per_unit = match unit.trim() {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        "y" => 365 * 86_400,
        "" => bail!("invalid duration {s:?}: add a unit (s, m, h, d, w or y)"),
        other => bail!("invalid duration {s:?}: unknown unit {other:?} (use s, m, h, d, w or y)"),
    };
    let secs = n
        .checked_mul(seconds_per_unit)
        .with_context(|| format!("duration {s:?} is too large"))?;
    Ok(Duration::from_secs(secs))
}

/// Describes how long ago something happened, e.g. `3h ago` or `41d ago`.
pub fn format_age(seconds_ago: i64) -> String {
    match seconds_ago {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 48 * 3600 => format!("{}h ago", s / 3600),
        s if s < 730 * 86_400 => format!("{}d ago", s / 86_400),
        s => format!("{}y ago", s / (365 * 86_400)),
    }
}

/// A duration such as `2h 05m`, `5m 12s` or `12s`.
pub fn format_duration(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Hours for reports: one decimal below 10, otherwise whole hours with separators.
pub fn format_hours(hours: f64) -> String {
    if hours < 10.0 {
        format!("{hours:.1}")
    } else {
        format_count(hours.round() as u64)
    }
}

/// A signed byte change such as `+1.2 GiB` or `-300.0 MiB`.
pub fn format_bytes_delta(delta: i128) -> String {
    let magnitude = format_bytes(delta.unsigned_abs().min(u128::from(u64::MAX)) as u64);
    match delta.signum() {
        1 => format!("+{magnitude}"),
        -1 => format!("-{magnitude}"),
        _ => magnitude,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(13_634_534_499_123), "12.4 TiB");
        assert_eq!(format_bytes(1024 * 1024 - 1), "1.0 MiB");
        assert_eq!(format_bytes(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(38_112_004), "38,112,004");
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("50GiB").unwrap(), 50 * 1024u64.pow(3));
        assert_eq!(parse_size("50G").unwrap(), 50 * 1024u64.pow(3));
        assert_eq!(parse_size("1.5 TiB").unwrap(), 3 * 1024u64.pow(4) / 2);
        assert_eq!(parse_size("500MB").unwrap(), 500_000_000);
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert!(parse_size("GiB").is_err());
        assert!(parse_size("5 parsecs").is_err());
        assert!(parse_size("99999999999999PiB").is_err());
    }

    #[test]
    fn parses_durations() {
        assert_eq!(
            parse_duration("30d").unwrap(),
            Duration::from_secs(30 * 86_400)
        );
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(
            parse_duration("2w").unwrap(),
            Duration::from_secs(14 * 86_400)
        );
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("d").is_err());
        assert_eq!(
            parse_duration("3y").unwrap(),
            Duration::from_secs(3 * 365 * 86_400)
        );
        assert!(parse_duration("3mo").is_err());
    }

    #[test]
    fn formats_ages() {
        assert_eq!(format_age(-5), "just now");
        assert_eq!(format_age(59), "just now");
        assert_eq!(format_age(600), "10m ago");
        assert_eq!(format_age(7200), "2h ago");
        assert_eq!(format_age(41 * 86_400), "41d ago");
        assert_eq!(format_age(3 * 365 * 86_400), "3y ago");
    }
}
