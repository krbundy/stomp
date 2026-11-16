//! Command implementations. Each module renders its own output.

pub mod doctor;
pub mod du;
pub mod init;
pub mod jobs;
pub mod report;
pub mod usage;

use std::fmt::Display;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::units;

/// Output format for commands that print data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Table,
    Csv,
    Json,
}

/// A timestamp in the local time zone, e.g. `2026-11-16 14:52 EST`.
pub(crate) fn local_time(ts: Timestamp) -> String {
    ts.to_zoned_datetime(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M %Z")
        .to_string()
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
