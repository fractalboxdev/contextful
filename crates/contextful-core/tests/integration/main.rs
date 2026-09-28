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
/// domain crate declares no workspace crate, dev builds included
/// (`topology.package.dependency-direction`), so its suites write the record and runner
/// stamp `contextful_eval::record::emit` writes, byte for byte, rather than calling it.
// mirrors: assurance.measure.record
pub fn emit(id: &str, value: f64, n: u64, seed: u64) {
    #[derive(serde::Serialize)]
    struct Runner {
        processor: String,
        nproc: u64,
        memory_limit: Option<u64>,
    }
    #[derive(serde::Serialize)]
    struct Record<'a> {
        id: &'a str,
        value: f64,
        n: u64,
        seed: u64,
        run: Runner,
    }
    let Some(dir) = std::env::var_os("CONTEXTFUL_MEASURE_DIR") else { return };
    let run = Runner {
        processor: processor_model(),
        nproc: std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(1),
        memory_limit: memory_limit(),
    };
    let record = Record { id, value, n, seed, run };
    let dir = std::path::PathBuf::from(dir);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(format!("{id}.json")), text + "\n")
    };
    write().unwrap_or_else(|e| panic!("writing the record of `{id}`: {e}"));
}

/// The processor model the operating system reports, as `contextful_eval::record::Runner`
/// reads it.
// mirrors: assurance.measure.runner-stamp
fn processor_model() -> String {
    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        let model = text
            .lines()
            .find(|l| l.starts_with("model name") || l.starts_with("Model") || l.starts_with("uarch"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string());
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            return m;
        }
    }
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| std::env::consts::ARCH.to_string())
}

/// The cgroup v2, then v1, memory limit; `max` and the v1 unlimited sentinel read as none.
// mirrors: assurance.measure.runner-stamp
fn memory_limit() -> Option<u64> {
    ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|t| t.trim().parse::<u64>().ok())
        .filter(|b| *b < (1 << 62))
}
