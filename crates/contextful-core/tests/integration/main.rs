//! The domain crate's one integration binary, one module per operation.

mod attenuate;
mod connector;
mod disclosure;
mod exchange;
mod grant;
mod identify;
mod memory;
mod issue;
mod pipeline;
mod read;
mod revoke;
mod run;
mod store;
mod time;

/// Record a ledger measure's figure when `contextful-ci measure` collects records. The
/// domain crate depends on no workspace crate, dev builds included, so its suites write
/// the record shape `contextful_eval::record` reads rather than calling it.
// mirrors: assurance.measure.record
pub fn emit(id: &str, value: f64, n: u64, seed: u64) {
    let Some(dir) = std::env::var_os("CONTEXTFUL_MEASURE_DIR") else { return };
    let nproc = std::thread::available_parallelism().map_or(1, |n| n.get());
    let record = serde_json::json!({
        "id": id,
        "value": value,
        "n": n,
        "seed": seed,
        "run": { "processor": std::env::consts::ARCH, "nproc": nproc, "memory_limit": null },
    });
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.json")), record.to_string()).unwrap();
}
