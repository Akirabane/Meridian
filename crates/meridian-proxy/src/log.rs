//! Log lines for people: one line per event on standard error, with the time in
//! UTC and the level, as `2026-11-02T17:04:05.123Z INFO  listening on ...`.
//! `MERIDIAN_LOG=debug` adds the debug lines.
use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
        }
    }
}

static MAX: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// Reads `MERIDIAN_LOG` (`error`, `warn`, `info` or `debug`). Anything else
/// keeps `info`.
pub fn init_from_env() {
    let level = match std::env::var("MERIDIAN_LOG").as_deref() {
        Ok("error") => Level::Error,
        Ok("warn") => Level::Warn,
        Ok("debug") => Level::Debug,
        _ => Level::Info,
    };
    MAX.store(level as u8, Ordering::Relaxed);
}

pub fn write(level: Level, args: fmt::Arguments<'_>) {
    if level as u8 > MAX.load(Ordering::Relaxed) {
        return;
    }
    let line = format!(
        "{} {} {args}\n",
        timestamp(SystemTime::now()),
        level.label()
    );
    // One write per line, so lines from several threads do not interleave.
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

/// The time as `YYYY-MM-DDTHH:MM:SS.mmmZ`, in UTC.
pub fn timestamp(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rest = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// The calendar date of a day counted from 1970-01-01, in the proleptic
/// Gregorian calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Error, format_args!($($arg)*)) };
}
macro_rules! warning {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Warn, format_args!($($arg)*)) };
}
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($arg)*)) };
}
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($arg)*)) };
}
pub(crate) use {debug, error, info, warning};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64, millis: u64) -> String {
        timestamp(UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis))
    }

    #[test]
    fn the_epoch_is_written_in_utc() {
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_known_time_is_written_with_milliseconds() {
        // 2026-11-02 17:04:05 UTC.
        assert_eq!(at(1_793_639_045, 123), "2026-11-02T17:04:05.123Z");
    }

    #[test]
    fn a_leap_day_is_a_real_date() {
        // 2024-02-29 00:00:00 UTC, then the next day.
        assert_eq!(at(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
        assert_eq!(at(1_709_164_800 + 86_400, 0), "2024-03-01T00:00:00.000Z");
    }

    #[test]
    fn levels_are_ordered_from_the_most_serious() {
        assert!(
            Level::Error < Level::Warn && Level::Warn < Level::Info && Level::Info < Level::Debug
        );
    }
}
