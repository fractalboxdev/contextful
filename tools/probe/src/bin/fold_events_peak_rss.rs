//! The peak resident set of a process folding one 260-row landing onto a snapshot of
//! 10^6 unkeyed, day-partitioned events, the median of five fresh children.

#[path = "../measure.rs"]
mod measure;
#[path = "../store_fixture.rs"]
mod store_fixture;
#[path = "../store_rss.rs"]
mod store_rss;

use contextful_eval::record;

const ID: &str = "fold-events-peak-rss";
const SEED: u64 = 0x5eed_00f5;

fn main() {
    let held = if measure::small() { 20_000 } else { 1_000_000 };
    if let Some(mib) = store_rss::run("events", held, SEED) {
        record::emit(ID, mib, held as u64, SEED);
    }
}
