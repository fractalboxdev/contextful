//! Vector sidecars at the sizes the workspace stage does not build: the resident memory of a
//! process probing a 200 MB mapped graph (`read.retrieve.sidecar-size-cap`), and recall@10
//! against exact search over 100,000 vectors, unfiltered and filtered
//! (`read.retrieve.sidecar-generates-candidates`, `read.retrieve.filtered-traversal`), for
//! each of `f32`, `f16` and `i8` storage.
//!
//! Each sidecar is built by `vector::build`, the builder the fold calls over its staged rows.
//! A resident case builds the sidecar, then runs this binary again as a fresh child that
//! opens it, probes it once and reports its own peak resident set, so the build's memory
//! never counts.
//!
//! `cargo bench -p contextful-context --bench vector_sidecar` runs both parts: recall in
//! about 2 minutes on a 28-core workstation, and resident memory in about 70, almost all of
//! it the single-threaded graph builds; `-- resident` or `-- recall` runs one. Every case
//! prints its figures; the workspace test stage builds no bench target.

use arrow_array::{ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use contextful_context::vector::{self, FilteredPlan, Sealing, VectorSidecar, GRAPH_FILE};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::index::{IndexEntry, VectorEntry, VectorStorage};
use contextful_core::store::lay_out::{SnapshotId, MANIFEST_FILE};
use contextful_eval::record::emit;
use contextful_eval::trend::percentile;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant as Clock;

/// The seed every resident-case vector and query derives from.
const RESIDENT_SEED: u64 = 0x5eed_0044;
/// The resident cases' dimension.
const RESIDENT_DIM: usize = 1024;
/// Stored vector bytes of a resident case: 200 MB.
const RESIDENT_BYTES: usize = 200_000_000;
/// Probes a resident child runs after its first.
const RESIDENT_PROBES: usize = 200;
/// The resident cases' construction width: the default `m` and a candidate list of 64
/// rather than the default 200, which builds the `f16` case in 24 minutes rather than 32 on
/// a 28-core workstation. A probe searches at `EF_SEARCH_FLOOR` either way.
const RESIDENT_GRAPH: &str = "ef_construction = 64\n";

/// The recall fixture's seed, size, dimension and query count: `index.rs`'s recall
/// fixture at 50 times its rows.
const RECALL_SEED: u64 = 0x5eed_0043;
const RECALL_N: usize = 100_000;
const RECALL_DIM: usize = 32;
const RECALL_QUERIES: usize = 200;
/// Filter shares of the recall fixture: 1% holds 1,000 rows, under the exact threshold,
/// and 10% holds 10,000, which the probe traverses.
const FILTER_PERCENTS: [usize; 2] = [1, 10];

/// `n` vectors of `dim` drawn from `seed`, each element in [-1, 1), row-major: the
/// generator `index.rs`'s recall fixture draws from.
fn seeded(n: usize, dim: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..n * dim)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        })
        .collect()
}

fn storage_name(storage: VectorStorage) -> &'static str {
    match storage {
        VectorStorage::F32 => "f32",
        VectorStorage::F16 => "f16",
        VectorStorage::I8 => "i8",
    }
}

/// Build the sidecar over `vectors` (`count * dim`, row-major) under a snapshot directory
/// in `root`, its vector block carrying `extra`, returning that directory and the entry its
/// manifest records.
fn build(root: &Path, vectors: &[f32], dim: usize, storage: VectorStorage, extra: &str) -> (std::path::PathBuf, VectorEntry) {
    let count = vectors.len() / dim;
    let decl = TableDecl::parse_pipeline(&format!(
        "[[pipeline.tables]]\nname = \"passages\"\nprimary_key = [\"passage_id\"]\n[[pipeline.tables.indexes]]\nkind = \"vector\"\ncolumn = \"embedding\"\nmodel = \"e5\"\ndim = {dim}\nstorage = \"{}\"\n{extra}",
        storage_name(storage)
    ))
    .unwrap()
    .remove(0);
    let index = decl.indexes.as_ref().unwrap()[0].clone();
    let item = Arc::new(Field::new("item", DataType::Float32, true));
    let embedding = FixedSizeListArray::new(item.clone(), dim as i32, Arc::new(Float32Array::from(vectors.to_vec())), None);
    let ids = StringArray::from_iter_values((0..count).map(|i| format!("p{i}")));
    let schema = Schema::new(vec![
        Field::new("passage_id", DataType::Utf8, false),
        Field::new("embedding", DataType::FixedSizeList(item, dim as i32), true),
    ]);
    let rows = RecordBatch::try_new(Arc::new(schema), vec![Arc::new(ids) as ArrayRef, Arc::new(embedding)]).unwrap();
    let snapshot_id = SnapshotId::try_from("snapshot-00000000000000000001".to_string()).unwrap();
    let dir = root.join(snapshot_id.to_string());
    let entry = vector::build(&dir, &snapshot_id, &rows, &decl, &index, &Sealing::Plaintext).unwrap();
    assert_eq!(entry.row_count, count as u64, "every seeded vector is non-zero and identified");
    (dir, entry)
}

fn open(dir: &Path, entry: &VectorEntry) -> VectorSidecar {
    let sidecar = VectorSidecar::open(dir, "passages", &IndexEntry::Vector(entry.clone()), &Sealing::Plaintext).unwrap();
    assert!(sidecar.is_mapped(), "a plaintext sidecar maps its graph");
    sidecar
}

// ---------------------------------------------------------------- resident memory

/// This process's resident set and its anonymous share (`RssAnon` on Linux, the physical
/// footprint on macOS), in bytes; the mapped graph's clean pages count in the first alone.
#[cfg(target_os = "macos")]
fn resident_now() -> (u64, u64) {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    let rc = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
    assert_eq!(rc, 0, "proc_pid_rusage failed");
    let info = unsafe { info.assume_init() };
    (info.ri_resident_size, info.ri_phys_footprint)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn resident_now() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kib = |key: &str| -> u64 {
        let line = status.lines().find(|l| l.starts_with(key)).unwrap_or_else(|| panic!("/proc/self/status lacks {key}"));
        line[key.len()..].trim().trim_end_matches("kB").trim().parse::<u64>().unwrap() * 1024
    };
    (kib("VmRSS:"), kib("RssAnon:"))
}

/// This process's peak resident set in bytes: `ru_maxrss` counts KiB on Linux and bytes on
/// macOS.
#[cfg(unix)]
fn peak_rss() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) }, 0, "getrusage failed");
    let max = unsafe { usage.assume_init() }.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        max
    } else {
        max * 1024
    }
}

const MIB: f64 = 1024.0 * 1024.0;

/// The child: open the sidecar at `dir`, probe it once, then `RESIDENT_PROBES` more times,
/// printing its resident set before opening, then peak resident set, resident set and
/// anonymous growth over the state before opening, in MiB, after the first probe and after
/// the rest.
#[cfg(unix)]
fn resident_child(dir: &Path, path: &str) {
    let entry: VectorEntry = serde_json::from_slice(&std::fs::read(dir.join(path).join(MANIFEST_FILE)).unwrap()).unwrap();
    let dim = entry.dim as usize;
    let queries = seeded(RESIDENT_PROBES + 1, dim, RESIDENT_SEED ^ 0xffff);
    let (rss_before, anon_before) = resident_now();
    println!("before {:.2}", rss_before as f64 / MIB);
    let sidecar = open(dir, &entry);
    let figures = |label: &str| {
        let (rss, anon) = resident_now();
        println!("{label} {:.2} {:.2} {:.2}", peak_rss() as f64 / MIB, rss as f64 / MIB, anon.saturating_sub(anon_before) as f64 / MIB);
    };
    let first = sidecar.probe(&queries[..dim], 10).unwrap();
    assert_eq!(first.len(), 10, "the probe answers");
    figures("one");
    let mut latency = Vec::with_capacity(RESIDENT_PROBES);
    for q in queries[dim..].chunks_exact(dim) {
        let started = Clock::now();
        assert_eq!(sidecar.probe(q, 10).unwrap().len(), 10);
        latency.push(started.elapsed().as_secs_f64() * 1e3);
    }
    figures("all");
    println!("p50 {:.3}", percentile(&latency, 50.0));
}

/// One resident case's figures, in MiB and milliseconds.
struct Resident {
    rows: usize,
    graph_mib: f64,
    before: f64,
    build_s: f64,
    one: [f64; 3],
    all: [f64; 3],
    probe_p50_ms: f64,
}

#[cfg(unix)]
fn resident(storage: VectorStorage) -> Resident {
    let rows = RESIDENT_BYTES.div_ceil(RESIDENT_DIM * storage.element_bytes() as usize);
    let root = tempfile::tempdir().unwrap();
    let started = Clock::now();
    let (dir, entry) = {
        let vectors = seeded(rows, RESIDENT_DIM, RESIDENT_SEED);
        build(root.path(), &vectors, RESIDENT_DIM, storage, RESIDENT_GRAPH)
    };
    let build_s = started.elapsed().as_secs_f64();
    assert!(entry.stored_vector_bytes() >= RESIDENT_BYTES as u64, "{} B stored", entry.stored_vector_bytes());
    let graph_mib = std::fs::metadata(dir.join(&entry.path).join(GRAPH_FILE)).unwrap().len() as f64 / MIB;
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--resident-child", dir.to_str().unwrap(), &entry.path])
        .env_remove(contextful_eval::record::MEASURE_DIR_VAR)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = |label: &str| -> Vec<f64> {
        let l = stdout.lines().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("the child printed no `{label}`: {stdout}"));
        l.split_whitespace().skip(1).map(|v| v.parse().unwrap()).collect()
    };
    let (one, all) = (line("one"), line("all"));
    Resident { rows, graph_mib, before: line("before")[0], build_s, one: [one[0], one[1], one[2]], all: [all[0], all[1], all[2]], probe_p50_ms: line("p50")[0] }
}

#[cfg(unix)]
fn resident_part() {
    for (storage, suffix) in [(VectorStorage::F16, ""), (VectorStorage::I8, "-i8")] {
        let r = resident(storage);
        println!(
            "resident {} at {} × {RESIDENT_DIM} ({:.1} MiB graph, built in {:.0} s): {:.1} MiB resident before opening; one probe peak {:.1} MiB, resident {:.1} MiB, anonymous +{:.2} MiB; after {} more, peak {:.1} MiB, resident {:.1} MiB, anonymous +{:.2} MiB; probe p50 {:.2} ms",
            storage_name(storage),
            r.rows,
            r.graph_mib,
            r.build_s,
            r.before,
            r.one[0],
            r.one[1],
            r.one[2],
            RESIDENT_PROBES,
            r.all[0],
            r.all[1],
            r.all[2],
            r.probe_p50_ms
        );
        // The reader's own memory, which the 64 MiB bound counts, and the whole process's
        // peak, mapped graph pages included.
        emit(&format!("vector-resident{suffix}"), r.one[2], 1, RESIDENT_SEED);
        emit(&format!("vector-resident-peak-rss{suffix}"), r.one[0], 1, RESIDENT_SEED);
    }
}

#[cfg(not(unix))]
fn resident_part() {
    println!("resident: the child reads getrusage, which this platform lacks; skipped");
}

// ---------------------------------------------------------------- recall

/// The rows of `n` a filter holding `percent` of them matches, spread evenly over the
/// graph: `index.rs`'s `matching`.
fn matching(n: usize, percent: usize) -> HashSet<String> {
    (0..n).filter(|i| i * 37 % 100 < percent).map(|i| format!("p{i}")).collect()
}

/// The ids of the 10 rows `admits` keeps nearest `q` by exact `Float32` cosine.
fn exact_top_10(vectors: &[f32], dim: usize, q: &[f32], admits: impl Fn(usize) -> bool) -> HashSet<String> {
    let qn = q.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mut scored: Vec<(f32, usize)> = vectors
        .chunks_exact(dim)
        .enumerate()
        .filter(|(i, _)| admits(*i))
        .map(|(i, v)| {
            let dot: f32 = q.iter().zip(v).map(|(a, b)| a * b).sum();
            (dot / (qn * v.iter().map(|x| x * x).sum::<f32>().sqrt()), i)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored[..10].iter().map(|(_, i)| format!("p{i}")).collect()
}

fn recall_part() {
    let vectors = seeded(RECALL_N, RECALL_DIM, RECALL_SEED);
    let queries = seeded(RECALL_N + RECALL_QUERIES, RECALL_DIM, RECALL_SEED).split_off(RECALL_N * RECALL_DIM);
    let queries: Vec<&[f32]> = queries.chunks_exact(RECALL_DIM).collect();
    let filters: Vec<(usize, HashSet<String>)> = FILTER_PERCENTS.iter().map(|&p| (p, matching(RECALL_N, p))).collect();
    let truth: Vec<HashSet<String>> = queries.iter().map(|q| exact_top_10(&vectors, RECALL_DIM, q, |_| true)).collect();
    let filtered_truth: Vec<Vec<HashSet<String>>> = filters
        .iter()
        .map(|(p, _)| queries.iter().map(|q| exact_top_10(&vectors, RECALL_DIM, q, |i| i * 37 % 100 < *p)).collect())
        .collect();
    for storage in [VectorStorage::F32, VectorStorage::F16, VectorStorage::I8] {
        let root = tempfile::tempdir().unwrap();
        let started = Clock::now();
        let (dir, entry) = build(root.path(), &vectors, RECALL_DIM, storage, "");
        let build_s = started.elapsed().as_secs_f64();
        let sidecar = open(&dir, &entry);
        let name = storage_name(storage);
        let suffix = if storage == VectorStorage::F32 { String::new() } else { format!("-{name}") };

        let mut hits = 0usize;
        for (q, t) in queries.iter().zip(&truth) {
            let found = sidecar.probe(q, 10).unwrap();
            assert_eq!(found.len(), 10);
            hits += found.iter().filter(|c| t.contains(&c.id)).count();
        }
        let recall = hits as f64 / (RECALL_QUERIES * 10) as f64;
        println!("recall {name} at {RECALL_N} × {RECALL_DIM} (built in {build_s:.1} s): unfiltered recall@10 {recall:.4}");
        emit(&format!("vector-recall-100k{suffix}"), recall, RECALL_QUERIES as u64, RECALL_SEED);

        for ((percent, admitted), truths) in filters.iter().zip(&filtered_truth) {
            let (mut hits, mut plans) = (0usize, HashSet::new());
            for (q, t) in queries.iter().zip(truths) {
                let (found, plan) = sidecar.probe_filtered(q, 10, None, admitted).unwrap();
                assert_eq!(found.len(), 10, "{percent}%: the probe returns k admitted rows");
                assert!(found.iter().all(|c| admitted.contains(&c.id)), "{percent}%: {found:?}");
                hits += found.iter().filter(|c| t.contains(&c.id)).count();
                plans.insert(format!("{plan:?}"));
            }
            assert_eq!(plans.len(), 1, "{percent}%: {plans:?}");
            let expected = FilteredPlan::for_matching(admitted.len() as u64);
            assert!(plans.contains(&format!("{expected:?}")), "{percent}%: {plans:?}");
            let recall = hits as f64 / (RECALL_QUERIES * 10) as f64;
            println!("recall {name} filtered to {percent}% ({} rows, {expected:?}): recall@10 {recall:.4}", admitted.len());
            emit(&format!("vector-filtered-recall-100k-{percent}pct{suffix}"), recall, RECALL_QUERIES as u64, RECALL_SEED);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(unix)]
    if args.get(1).map(String::as_str) == Some("--resident-child") {
        resident_child(Path::new(&args[2]), &args[3]);
        return;
    }
    // `cargo bench` passes `--bench` ahead of the caller's arguments; only part names count.
    let parts: Vec<&str> = args.iter().skip(1).map(String::as_str).filter(|a| matches!(*a, "resident" | "recall")).collect();
    if parts.is_empty() || parts.contains(&"recall") {
        recall_part();
    }
    if parts.is_empty() || parts.contains(&"resident") {
        resident_part();
    }
}
