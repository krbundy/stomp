//! Command implementations. Each module renders its own output.

pub mod archive;
pub mod candidates;
pub mod changes;
pub mod doctor;
pub mod du;
pub mod init;
pub mod jobs;
pub mod progress;
pub mod report;
pub mod scan;
pub mod schedule;
pub mod tasks;
pub mod usage;

use std::fmt::Display;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;

use crate::catalog::Catalog;
use crate::table::{Table, csv_from_rows};
use crate::units;

/// Output format for commands that print data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Table,
    Csv,
    Json,
    /// Markdown, for pasting into documents and issues
    Md,
}

/// A timestamp in the local time zone, e.g. `2026-11-16 14:52 EST`.
pub(crate) fn local_time(ts: Timestamp) -> String {
    ts.to_zoned_datetime(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M %Z")
        .to_string()
}

/// An RFC 3339 timestamp in local time without the zone, e.g. `2026-11-16 14:52`.
pub(crate) fn short_local(rfc3339: &str) -> String {
    match rfc3339.parse::<Timestamp>() {
        Ok(ts) => ts
            .to_zoned_datetime(TimeZone::system())
            .strftime("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// Unix seconds as a local date, e.g. `2026-11-16`.
pub(crate) fn local_date(seconds: i64) -> String {
    Timestamp::from_second(seconds)
        .map(|t| t.to_zoned_datetime(TimeZone::system()).date().to_string())
        .unwrap_or_default()
}

/// How long ago a change happened, or a dash if unknown.
pub(crate) fn age(newest_change: Option<i64>) -> String {
    match newest_change {
        Some(t) => units::format_age(Timestamp::now().as_second() - t),
        None => "—".to_string(),
    }
}

/// Unix seconds as RFC 3339, or an empty string.
pub(crate) fn rfc3339(seconds: Option<i64>) -> String {
    seconds
        .and_then(|s| Timestamp::from_second(s).ok())
        .map(|t| t.to_string())
        .unwrap_or_default()
}

pub(crate) fn warn(message: impl Display) {
    eprintln!("stomp: warning: {message}");
}

/// Asks a yes/no question on the terminal (§12.4, I5). Without a terminal,
/// destructive commands need `--yes` instead.
pub(crate) fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("this needs confirmation; run it again with --yes");
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Opens the catalog and imports anything waiting in the inbox (§10.2).
pub(crate) fn open_catalog(data_dir: &Path) -> Result<Catalog> {
    let mut catalog = Catalog::open(data_dir)?;
    for message in catalog.ingest(data_dir)? {
        warn(message);
    }
    Ok(catalog)
}

/// Prints rows as JSON or CSV, or the table built by `table` as text or Markdown.
pub(crate) fn emit<T: Serialize>(
    format: Format,
    rows: &[T],
    table: impl FnOnce() -> Table,
) -> Result<()> {
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(rows)?),
        Format::Csv => print!("{}", csv_from_rows(rows)?),
        Format::Table => print!("{}", table().render()),
        Format::Md => print!("{}", table().render_markdown()),
    }
    Ok(())
}

/// The start of a local day as Unix seconds, optionally the day after.
pub(crate) fn day_start(text: &str, next_day: bool) -> Result<i64> {
    let date: Date = text
        .parse()
        .with_context(|| format!("{text:?} is not a date; use YYYY-MM-DD"))?;
    let date = if next_day { date.tomorrow()? } else { date };
    Ok(date
        .to_zoned_datetime(TimeZone::system())?
        .timestamp()
        .as_second())
}

/// A point in time given as an age (`30d`) or a date (`2026-01-31`), as Unix seconds.
pub(crate) fn parse_since(text: &str) -> Result<i64> {
    match units::parse_duration(text) {
        Ok(age) => Ok(Timestamp::now().as_second() - age.as_secs() as i64),
        Err(_) => day_start(text, false).with_context(|| {
            format!("{text:?} is neither an age such as 30d nor a date such as 2026-01-31")
        }),
    }
}
