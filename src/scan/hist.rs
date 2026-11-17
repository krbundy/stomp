//! Aggregates for a directory subtree: totals plus file-size and
//! modification-month histograms (§9.3).
//!
//! Ages are stored as absolute months rather than relative buckets, so the
//! aggregates of an unchanged subtree stay correct when a later incremental
//! scan reuses them; ages relative to "now" are computed at report time.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::fs::Usage;

/// Upper bounds of the file-size buckets; the last bucket is everything larger.
pub const SIZE_BOUNDS: [u64; 4] = [4 << 10, 1 << 20, 100 << 20, 10 << 30];
pub const SIZE_LABELS: [&str; 5] = [
    "under 4 KiB",
    "4 KiB–1 MiB",
    "1–100 MiB",
    "100 MiB–10 GiB",
    "over 10 GiB",
];
pub const AGE_LABELS: [&str; 5] = [
    "under 30 days",
    "30–90 days",
    "90 days–1 year",
    "1–3 years",
    "over 3 years",
];
const AGE_BOUNDS_DAYS: [i64; 4] = [30, 90, 365, 3 * 365];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Count {
    pub files: u64,
    pub bytes: u64,
}

impl Count {
    pub fn add(&mut self, files: u64, bytes: u64) {
        self.files += files;
        self.bytes += bytes;
    }

    pub fn merge(&mut self, other: Count) {
        self.add(other.files, other.bytes);
    }
}

/// Totals and histograms for a directory subtree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Agg {
    pub bytes: u64,
    pub files: u64,
    /// Entries that couldn't be read.
    pub unreadable: u64,
    pub newest_mtime: Option<i64>,
    pub oldest_mtime: Option<i64>,
    /// Newest ctime of any entry, including directories.
    pub newest_change: Option<i64>,
    pub sizes: [Count; 5],
    /// Keyed by [`month_index`] of each file's modification time.
    pub months: BTreeMap<u32, Count>,
}

fn max_opt(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
}

fn min_opt(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

impl Agg {
    pub fn add_file(&mut self, len: u64, mtime: i64, ctime: i64) {
        self.bytes += len;
        self.files += 1;
        self.newest_mtime = max_opt(self.newest_mtime, Some(mtime));
        self.oldest_mtime = min_opt(self.oldest_mtime, Some(mtime));
        self.saw_change(ctime);
        self.sizes[size_bucket(len)].add(1, len);
        self.months
            .entry(month_index(mtime))
            .or_default()
            .add(1, len);
    }

    pub fn saw_change(&mut self, t: i64) {
        self.newest_change = max_opt(self.newest_change, Some(t));
    }

    pub fn merge(&mut self, other: &Agg) {
        self.bytes += other.bytes;
        self.files += other.files;
        self.unreadable += other.unreadable;
        self.newest_mtime = max_opt(self.newest_mtime, other.newest_mtime);
        self.oldest_mtime = min_opt(self.oldest_mtime, other.oldest_mtime);
        self.newest_change = max_opt(self.newest_change, other.newest_change);
        for (mine, theirs) in self.sizes.iter_mut().zip(other.sizes) {
            mine.merge(theirs);
        }
        for (month, count) in &other.months {
            self.months.entry(*month).or_default().merge(*count);
        }
    }

    /// Totals only, for a directory measured without walking it.
    pub fn from_usage(u: &Usage) -> Agg {
        Agg {
            bytes: u.bytes,
            files: u.files,
            unreadable: u.unreadable,
            newest_change: u.newest_change,
            ..Agg::default()
        }
    }
}

pub fn size_bucket(len: u64) -> usize {
    SIZE_BOUNDS
        .iter()
        .position(|&bound| len < bound)
        .unwrap_or(SIZE_BOUNDS.len())
}

// Calendar conversions (Howard Hinnant's algorithms), so the scanner needn't
// build a timezone-aware value for every file.

fn civil_from_days(days: i64) -> (i64, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month)
}

fn days_from_civil(year: i64, month: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5; // the first of the month
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Months since January of year 0 (UTC) for a Unix time; earlier times clamp to 0.
pub fn month_index(unix_secs: i64) -> u32 {
    let (year, month) = civil_from_days(unix_secs.div_euclid(86_400));
    u32::try_from(year * 12 + i64::from(month) - 1).unwrap_or(0)
}

/// Unix time of the first moment of a month index.
pub fn month_start(index: u32) -> i64 {
    let year = i64::from(index / 12);
    let month = index % 12 + 1;
    days_from_civil(year, month) * 86_400
}

/// A month index as `YYYY-MM`.
pub fn month_label(index: u32) -> String {
    format!("{:04}-{:02}", index / 12, index % 12 + 1)
}

/// Splits the data by age relative to `now`, using each month's midpoint, so
/// it is accurate to within about two weeks.
pub fn age_profile(months: &BTreeMap<u32, Count>, now: i64) -> [Count; 5] {
    let mut out = [Count::default(); 5];
    for (&month, &count) in months {
        let age_days = (now - (month_start(month) + 15 * 86_400)) / 86_400;
        let bucket = AGE_BOUNDS_DAYS
            .iter()
            .position(|&bound| age_days < bound)
            .unwrap_or(AGE_BOUNDS_DAYS.len());
        out[bucket].merge(count);
    }
    out
}

// Compact encoding for the catalog: unsigned LEB128 varints.

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn get_varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let (&byte, rest) = input.split_first()?;
        *input = rest;
        if shift > 63 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
}

pub fn encode_sizes(sizes: &[Count; 5]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in sizes {
        put_varint(&mut out, c.files);
        put_varint(&mut out, c.bytes);
    }
    out
}

/// Decodes size buckets; malformed data decodes as empty.
pub fn decode_sizes(mut data: &[u8]) -> [Count; 5] {
    let mut out = [Count::default(); 5];
    for c in &mut out {
        match (get_varint(&mut data), get_varint(&mut data)) {
            (Some(files), Some(bytes)) => *c = Count { files, bytes },
            _ => return [Count::default(); 5],
        }
    }
    out
}

pub fn encode_months(months: &BTreeMap<u32, Count>) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, months.len() as u64);
    let mut previous = 0u32;
    for (&month, c) in months {
        put_varint(&mut out, u64::from(month - previous));
        put_varint(&mut out, c.files);
        put_varint(&mut out, c.bytes);
        previous = month;
    }
    out
}

/// Decodes a month histogram; malformed data decodes as empty.
pub fn decode_months(mut data: &[u8]) -> BTreeMap<u32, Count> {
    let decode = |data: &mut &[u8]| -> Option<BTreeMap<u32, Count>> {
        let len = get_varint(data)?;
        let mut out = BTreeMap::new();
        let mut month = 0u32;
        for _ in 0..len {
            month = month.checked_add(u32::try_from(get_varint(data)?).ok()?)?;
            let files = get_varint(data)?;
            let bytes = get_varint(data)?;
            out.insert(month, Count { files, bytes });
        }
        Some(out)
    };
    decode(&mut data).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_index_matches_calendar() {
        for (text, expected) in [
            ("1970-01-01T00:00:00Z", "1970-01"),
            ("2000-02-29T12:00:00Z", "2000-02"),
            ("2026-11-17T23:59:59Z", "2026-11"),
            ("2024-12-31T23:59:59Z", "2024-12"),
            ("2025-01-01T00:00:00Z", "2025-01"),
            ("1969-12-31T23:59:59Z", "1969-12"),
        ] {
            let ts: jiff::Timestamp = text.parse().unwrap();
            let index = month_index(ts.as_second());
            assert_eq!(month_label(index), expected, "{text}");
            let start: jiff::Timestamp = format!("{expected}-01T00:00:00Z").parse().unwrap();
            assert_eq!(month_start(index), start.as_second(), "{text}");
        }
    }

    #[test]
    fn aggregates_files_and_merges() {
        let jan_2025 = 1_735_689_600; // 2025-01-01
        let mut a = Agg::default();
        a.add_file(100, jan_2025, jan_2025 + 10);
        a.add_file(5 << 20, jan_2025 + 40 * 86_400, jan_2025);
        assert_eq!(a.bytes, 100 + (5 << 20));
        assert_eq!(
            a.sizes[0],
            Count {
                files: 1,
                bytes: 100
            }
        );
        assert_eq!(a.sizes[2].files, 1);
        assert_eq!(a.months.len(), 2);
        assert_eq!(a.newest_change, Some(jan_2025 + 10));

        let mut b = Agg::default();
        b.add_file(1, jan_2025 - 86_400, jan_2025 - 86_400);
        b.unreadable = 2;
        a.merge(&b);
        assert_eq!(a.files, 3);
        assert_eq!(a.unreadable, 2);
        assert_eq!(a.oldest_mtime, Some(jan_2025 - 86_400));
        assert_eq!(a.months.len(), 3);
    }

    #[test]
    fn encodes_round_trip() {
        let mut a = Agg::default();
        for (len, t) in [(0, 0), (1 << 40, 1_700_000_000), (12345, 1_600_000_000)] {
            a.add_file(len, t, t);
        }
        assert_eq!(decode_sizes(&encode_sizes(&a.sizes)), a.sizes);
        assert_eq!(decode_months(&encode_months(&a.months)), a.months);
        assert_eq!(decode_months(&[0xff]), BTreeMap::new());
        assert_eq!(decode_sizes(&[1, 2]), [Count::default(); 5]);
    }

    #[test]
    fn splits_by_age() {
        let now: jiff::Timestamp = "2026-11-17T00:00:00Z".parse().unwrap();
        let mut months = BTreeMap::new();
        // Ages are measured from each month's middle: mid-September is 62 days back.
        for (date, bytes) in [
            ("2026-11-01", 1),
            ("2026-09-01", 10),
            ("2026-01-01", 100),
            ("2024-06-01", 1000),
            ("2019-01-01", 10000),
        ] {
            let ts: jiff::Timestamp = format!("{date}T00:00:00Z").parse().unwrap();
            months.insert(month_index(ts.as_second()), Count { files: 1, bytes });
        }
        let profile = age_profile(&months, now.as_second());
        let bytes: Vec<u64> = profile.iter().map(|c| c.bytes).collect();
        assert_eq!(bytes, [1, 10, 100, 1000, 10000]);
    }
}
