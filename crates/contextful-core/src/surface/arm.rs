//! The schedule grammar, the cron dialect and an entry's next fire (`surface.arm`).
//!
//! A schedule is `every <n><unit>` or a five-field UTC cron expression
//! (`surface.arm.grammar`). The next fire counts from the run history, so a daemon that
//! boots past it fires once and re-arms from that run (`surface.arm.catch-up`).

use super::SurfaceError;
use crate::time::Instant;
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time};

/// The in-process adapter's evaluation interval (`surface.arm.tick-interval`).
pub const TICK_INTERVAL_MS: u64 = 500;

/// Days a cron search walks before it gives up: 28 years, one full weekday-and-leap-year
/// cycle, so any expression that fires at all fires inside it.
const SEARCH_DAYS: u32 = 28 * 366;

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// A fixed interval, in seconds.
    Every(u64),
    Cron(Cron),
}

/// A five-field cron expression, each field a bit set of admitted values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    /// Bit 0 is Sunday; a written 7 folds onto it.
    dow: u64,
    dom_restricted: bool,
    dow_restricted: bool,
}

impl Schedule {
    /// Read `s`, refusing anything outside the grammar with `ScheduleUnreadable`
    /// (`surface.arm.unreadable-schedule`).
    pub fn parse(s: &str) -> Result<Schedule, SurfaceError> {
        let unreadable = |why: String| SurfaceError::ScheduleUnreadable(format!("`{s}`: {why}"));
        if let Some(rest) = s.strip_prefix("every") {
            return every(rest.trim_start()).map(Schedule::Every).map_err(unreadable);
        }
        Cron::parse(s).map(Schedule::Cron).map_err(unreadable)
    }

    /// The first instant this schedule admits strictly after `t`.
    pub fn next_after(&self, t: Instant) -> Instant {
        match self {
            Schedule::Every(secs) => t.plus_secs(*secs),
            Schedule::Cron(c) => c.next_after(t),
        }
    }
}

/// An armed entry's next fire: the first instant `schedule` admits after the later of the
/// pipeline's last journaled run start and its last dispatch, or `armed_at` with neither
/// (`surface.arm.next-fire-from-history`).
pub fn next_fire(schedule: &Schedule, last_run: Option<Instant>, last_dispatch: Option<Instant>, armed_at: Instant) -> Instant {
    match last_run.max(last_dispatch) {
        Some(base) => schedule.next_after(base),
        None => armed_at,
    }
}

fn every(rest: &str) -> Result<u64, String> {
    let split = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (n, unit) = rest.split_at(split);
    let n: u64 = n.parse().map_err(|_| "`every` takes a count and a unit, as `every 30s`".to_string())?;
    if n == 0 {
        return Err("an interval of zero never advances".into());
    }
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        other => return Err(format!("unit `{other}` is none of `s`, `m`, `h`, `d`")),
    };
    n.checked_mul(scale).ok_or_else(|| "the interval overflows".to_string())
}

/// Days in `month` of a leap year: the most a day-of-month field can reach in it.
fn max_days(month: u32) -> u32 {
    match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Cron {
    fn parse(s: &str) -> Result<Cron, String> {
        if s.starts_with('@') {
            return Err("`@` macros are outside the dialect".into());
        }
        let fields: Vec<&str> = s.split_whitespace().collect();
        let [minute, hour, dom, month, dow] = fields.as_slice() else {
            return Err(format!("{} fields; a cron expression has five: minute, hour, day of month, month, day of week", fields.len()));
        };
        let mut dow_bits = field(dow, 0, 7, "day of week")?;
        if dow_bits & (1 << 7) != 0 {
            dow_bits = (dow_bits & !(1 << 7)) | 1;
        }
        let cron = Cron {
            minute: field(minute, 0, 59, "minute")?,
            hour: field(hour, 0, 23, "hour")?,
            dom: field(dom, 1, 31, "day of month")?,
            month: field(month, 1, 12, "month")?,
            dow: dow_bits,
            dom_restricted: !dom.starts_with('*'),
            dow_restricted: !dow.starts_with('*'),
        };
        let reachable = (1..=12).filter(|m| cron.month & (1 << m) != 0).any(|m| (1..=max_days(m)).any(|d| cron.dom & (1 << d) != 0));
        if cron.dom_restricted && !cron.dow_restricted && !reachable {
            return Err("no month it names holds a day it names, so it never fires".into());
        }
        Ok(cron)
    }

    fn day_matches(&self, date: Date) -> bool {
        if self.month & (1 << u8::from(date.month())) == 0 {
            return false;
        }
        let dom = self.dom & (1 << date.day()) != 0;
        let dow = self.dow & (1 << date.weekday().number_days_from_sunday()) != 0;
        if self.dom_restricted && self.dow_restricted {
            dom || dow
        } else {
            dom && dow
        }
    }

    fn next_after(&self, t: Instant) -> Instant {
        // The first whole minute strictly after `t`.
        let start_secs = t.unix_secs().div_euclid(60) * 60 + 60;
        let start = OffsetDateTime::from_unix_timestamp(start_secs).unwrap_or(OffsetDateTime::UNIX_EPOCH);
        let mut date = start.date();
        for day in 0..SEARCH_DAYS {
            if self.day_matches(date) {
                let (h0, m0) = if day == 0 { (u32::from(start.hour()), u32::from(start.minute())) } else { (0, 0) };
                for h in (h0..24).filter(|h| self.hour & (1 << h) != 0) {
                    let from = if h == h0 { m0 } else { 0 };
                    if let Some(m) = (from..60).find(|m| self.minute & (1 << m) != 0) {
                        let time = Time::from_hms(h as u8, m as u8, 0).unwrap_or(Time::MIDNIGHT);
                        let secs = PrimitiveDateTime::new(date, time).assume_utc().unix_timestamp();
                        return Instant::from_unix_secs(secs).unwrap_or(t);
                    }
                }
            }
            match date.next_day() {
                Some(d) => date = d,
                None => break,
            }
        }
        // Parsing refuses every expression that never fires, so the search always returns
        // above; the far end of the representable range stands in for "never".
        t.plus_secs(u64::MAX)
    }
}

/// One cron field as a bit set over `lo..=hi`.
fn field(text: &str, lo: u32, hi: u32, name: &str) -> Result<u64, String> {
    let mut bits = 0u64;
    for item in text.split(',') {
        let bad = |why: &str| format!("{name} `{item}` {why}");
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (r, Some(s.parse::<u32>().map_err(|_| bad("carries an unreadable step"))?)),
            None => (item, None),
        };
        if step == Some(0) {
            return Err(bad("steps by zero"));
        }
        let value = |v: &str| -> Result<u32, String> {
            let n: u32 = v.parse().map_err(|_| bad("is not a number, `*`, a range, a list or a step"))?;
            if n < lo || n > hi {
                return Err(bad(&format!("falls outside {lo}-{hi}")));
            }
            Ok(n)
        };
        let (from, to) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (value(a)?, value(b)?);
            if a > b {
                return Err(bad("runs backwards"));
            }
            (a, b)
        } else {
            let a = value(range)?;
            (a, if step.is_some() { hi } else { a })
        };
        let step = step.unwrap_or(1);
        let mut v = from;
        while v <= to {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}
