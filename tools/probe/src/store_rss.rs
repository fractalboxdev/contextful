//! The resident-set fold probes: one child builds the store and exits; each of five fresh
//! children opens a copy, folds the 260-row increment and reports its own peak resident set.

use crate::measure;
use crate::store_fixture::{self, Table};

const CHILDREN: usize = 5;

/// Run the probe for `table` holding `held` rows, or serve as one of its children; the
/// median peak resident set in MiB, or `None` in a child.
pub fn run(table: &str, held: usize, seed: u64) -> Option<f64> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--build") => {
            store_fixture::build(std::path::Path::new(&args[2]), Table::parse(table), held, seed);
            None
        }
        Some("--fold") => {
            store_fixture::fold_increment(std::path::Path::new(&args[2]), Table::parse(table));
            println!("{}", measure::peak_rss_mib());
            None
        }
        _ => {
            let dir = tempfile::tempdir().unwrap();
            let built = dir.path().join("built");
            measure::child(&["--build", built.to_str().unwrap()]);
            let peaks = (0..CHILDREN)
                .map(|i| {
                    let copy = dir.path().join(format!("fold-{i}"));
                    measure::copy_tree(&built, &copy);
                    let peak = measure::child(&["--fold", copy.to_str().unwrap()]).parse().unwrap();
                    std::fs::remove_dir_all(&copy).unwrap();
                    peak
                })
                .collect();
            Some(measure::median(peaks))
        }
    }
}
