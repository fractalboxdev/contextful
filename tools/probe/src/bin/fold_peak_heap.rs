//! A fresh child process per held size measures the peak heap of the pass folding one
//! 260-row landing onto a snapshot of that many rows; the figure is the large size's peak
//! over the small one's.

#[path = "../measure.rs"]
mod measure;
#[path = "../store_fixture.rs"]
mod store_fixture;

use contextful_eval::record;
use store_fixture::Table;

const ID: &str = "fold-peak-heap-growth";
const SEED: u64 = 0x5eed_00f4;

#[global_allocator]
static ALLOCATOR: measure::PeakAllocator = measure::PeakAllocator;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--measure") {
        let held: usize = args.get(2).expect("a held row count").parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        store_fixture::build(dir.path(), Table::Events, held, SEED);
        let (peak, ()) = measure::peak_heap_of(|| store_fixture::fold_increment(dir.path(), Table::Events));
        println!("{peak}");
        return;
    }
    let (small, large) = if measure::small() { (2_000, 20_000) } else { (20_000, 200_000) };
    let peak = |held: usize| measure::child(&["--measure", &held.to_string()]).parse::<f64>().unwrap();
    let growth = peak(large) / peak(small);
    record::emit(ID, growth, large as u64, SEED);
}
