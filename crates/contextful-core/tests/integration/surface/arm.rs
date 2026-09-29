//! `surface.arm`: the schedule grammar, the cron dialect and an entry's next fire.

use contextful_core::surface::arm::{next_fire, Schedule, TICK_INTERVAL_MS};
use contextful_core::time::Instant;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn next(schedule: &str, after: &str) -> String {
    Schedule::parse(schedule).unwrap_or_else(|e| panic!("{schedule}: {e}")).next_after(at(after)).to_rfc3339()
}

/// A schedule is `every <n><unit>` with unit `s`, `m`, `h` or `d`, or a UTC cron expression of five fields,
/// minute, hour, day of month, month and day of week, each `*`, a value, range, list or `/` step.
// spec: surface.arm.grammar@6644e0f9
#[test]
fn a_schedule_is_an_interval_or_a_five_field_cron() {
    assert_eq!(next("every 30s", "2030-01-01T00:00:00Z"), "2030-01-01T00:00:30Z");
    assert_eq!(next("every 15m", "2030-01-01T00:00:00Z"), "2030-01-01T00:15:00Z");
    assert_eq!(next("every 1h", "2030-01-01T00:00:00Z"), "2030-01-01T01:00:00Z");
    assert_eq!(next("every 2d", "2030-01-01T00:00:00Z"), "2030-01-03T00:00:00Z");
    // A value: 03:00 each day, strictly after the instant.
    assert_eq!(next("0 3 * * *", "2030-01-01T02:59:59Z"), "2030-01-01T03:00:00Z");
    assert_eq!(next("0 3 * * *", "2030-01-01T03:00:00Z"), "2030-01-02T03:00:00Z");
    // A step: every 15 minutes.
    assert_eq!(next("*/15 * * * *", "2030-01-01T00:07:00Z"), "2030-01-01T00:15:00Z");
    // A range and a list.
    assert_eq!(next("0 9-17 * * *", "2030-01-01T17:30:00Z"), "2030-01-02T09:00:00Z");
    assert_eq!(next("0 0 1,15 * *", "2030-01-02T00:00:00Z"), "2030-01-15T00:00:00Z");
    // Month and day of week: 2030-01-01 is a Tuesday, so the first Monday is the 7th.
    assert_eq!(next("0 0 * * 1", "2030-01-01T00:00:00Z"), "2030-01-07T00:00:00Z");
    assert_eq!(next("0 0 1 6 *", "2030-01-01T00:00:00Z"), "2030-06-01T00:00:00Z");
    // Day of week 7 reads as Sunday, like 0.
    assert_eq!(next("0 0 * * 7", "2030-01-01T00:00:00Z"), "2030-01-06T00:00:00Z");
    // A ranged step.
    assert_eq!(next("10-50/20 * * * *", "2030-01-01T00:31:00Z"), "2030-01-01T00:50:00Z");
    // Both day fields restricted: either matches.
    assert_eq!(next("0 0 13 * 5", "2030-01-01T00:00:00Z"), "2030-01-04T00:00:00Z");
}

/// A schedule string the grammar cannot read, including `L`, `W`, `?`, `#` and `@` macros, raises
/// `ScheduleUnreadable` for that entry alone, naming the diagnostic; every other entry of the document arms.
// spec: surface.arm.unreadable-schedule@090d0c2d
#[test]
fn an_unreadable_schedule_names_its_diagnostic() {
    for bad in ["0 0 L * *", "0 0 15W * *", "0 0 ? * *", "0 0 * * 1#2", "@daily", "every", "every 5w", "every 0s", "0 0 * *", "60 * * * *", "0 0 31 2 *", "*/0 * * * *"] {
        let e = Schedule::parse(bad).expect_err(bad).to_string();
        assert!(e.starts_with("ScheduleUnreadable"), "{bad}: {e}");
        assert!(e.contains(bad), "the diagnostic names the string: {e}");
    }
    // The refusal is per string: its neighbours still read.
    assert!(Schedule::parse("every 1h").is_ok());
}

/// An armed entry's next fire is the first instant its schedule admits after the later of its pipeline's last
/// journaled run start and its last dispatch; an entry with neither is due when armed.
// spec: surface.arm.next-fire-from-history@6558f610
#[test]
fn the_next_fire_counts_from_the_run_history() {
    let hourly = Schedule::parse("every 1h").unwrap();
    let armed = at("2030-01-01T12:00:00Z");
    // No history: due when armed.
    assert_eq!(next_fire(&hourly, None, None, armed), armed);
    // A run at 11:40 arms 12:40, not 13:00 from boot.
    assert_eq!(next_fire(&hourly, Some(at("2030-01-01T11:40:00Z")), None, armed), at("2030-01-01T12:40:00Z"));
    // A later dispatch that journaled no run moves the base.
    assert_eq!(next_fire(&hourly, Some(at("2030-01-01T11:40:00Z")), Some(at("2030-01-01T11:50:00Z")), armed), at("2030-01-01T12:50:00Z"));
    // Cron counts from history too: the first 03:00 after the last run.
    let nightly = Schedule::parse("0 3 * * *").unwrap();
    assert_eq!(next_fire(&nightly, Some(at("2030-01-01T03:00:05Z")), None, armed), at("2030-01-02T03:00:00Z"));
}

/// A daemon arming an entry whose next fire has passed fires it once, whatever count of intervals elapsed, and
/// arms the following fire from that run.
#[test]
fn a_long_gap_is_one_due_instant() {
    let hourly = Schedule::parse("every 1h").unwrap();
    let last = at("2030-01-01T00:00:00Z");
    let boot = at("2030-01-01T05:30:00Z");
    // Five intervals elapsed; the entry reads due once, at its first missed instant.
    let due = next_fire(&hourly, Some(last), None, boot);
    assert_eq!(due, at("2030-01-01T01:00:00Z"));
    assert!(due <= boot);
    // The fire at boot is the new history: the following fire is an hour after it, not 02:00.
    assert_eq!(next_fire(&hourly, Some(boot), None, boot), at("2030-01-01T06:30:00Z"));
}

/// The in-process adapter evaluates the armed set every 500 ms.
// spec: surface.arm.tick-interval@a6574f55
#[test]
fn the_tick_is_500_ms() {
    assert_eq!(TICK_INTERVAL_MS, 500);
}
