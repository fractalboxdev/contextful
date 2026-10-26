//! Seeded merge cases compare the executable merge model with the store's
//! `merge_verified`, key by key: the entry, the tombstone and the refusal each key ends
//! with, or the whole merge's `SyncCursorConflict`.

use anyhow::{bail, Context, Result};
use clap::Args as ClapArgs;
use contextful_core::store::sync::{merge_verified, BucketManifest, Entry, HeldKeys, Tombstone, TOMBSTONE_TTL_SECS};
use contextful_core::store::StoreError;
use contextful_core::time::Instant;
use proptest::prelude::{any, prop_oneof, Just, Strategy};
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_SEED: u64 = 30_086;
/// The instant every case merges at: far enough past the epoch that each tombstone age is reachable.
const NOW: u64 = 100 * 86_400;

#[derive(ClapArgs)]
pub struct Args {
    /// The generator seed, printed with the result.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// Generated merge cases.
    #[arg(long, default_value_t = 512)]
    cases: usize,
    /// A prebuilt protocol executable; absent, lake builds the package.
    #[arg(long)]
    reference: Option<PathBuf>,
}

/// One key's inputs: commit-log, held and verified flags, then the local entry, the remote
/// entry and the remote tombstone, each `(sha, owner)` or `(owner, deleted at)`.
#[derive(Clone, Debug)]
struct Key {
    log: bool,
    held: bool,
    verified: bool,
    local: Option<(u8, u8)>,
    remote: Option<(u8, u8)>,
    tomb: Option<(u8, u64)>,
}

#[derive(Clone, Debug)]
struct Case {
    me: u8,
    keys: Vec<Key>,
}

fn flag(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

impl Case {
    /// The case in the model executable's syntax.
    fn line(&self) -> String {
        let entry = |e: Option<(u8, u8)>| e.map(|(sha, owner)| format!("{sha}:{owner}")).unwrap_or_else(|| "-".into());
        let groups: Vec<String> = self
            .keys
            .iter()
            .map(|k| {
                let tomb = k.tomb.map(|(owner, at)| format!("{owner}@{at}")).unwrap_or_else(|| "-".into());
                format!("{} {} {} {} {} {tomb}", flag(k.log), flag(k.held), flag(k.verified), entry(k.local), entry(k.remote))
            })
            .collect();
        format!("me={} now={NOW} {}", self.me, groups.join(" ; "))
    }
}

fn owner(n: u8) -> String {
    if n == 0 {
        String::new()
    } else {
        format!("node-{n}")
    }
}

fn path(i: usize, log: bool) -> String {
    if log {
        format!("p/cursors/feed/{i:020}.json")
    } else {
        format!("p/tables/t/data/{i}.parquet")
    }
}

fn at(secs: u64) -> Result<Instant> {
    Instant::from_unix_secs(secs as i64).context("a case instant")
}

/// The store's merge of `case`, in the model executable's output syntax.
fn store(case: &Case) -> Result<String> {
    let mut remote = BucketManifest::default();
    let mut local = BTreeMap::new();
    let mut held = BTreeSet::new();
    let mut verified = BTreeSet::new();
    for (i, k) in case.keys.iter().enumerate() {
        let key = path(i, k.log);
        if let Some((sha, o)) = k.local {
            local.insert(key.clone(), Entry { sha256: sha.to_string(), size: 1, owner: owner(o) });
        }
        if let Some((sha, o)) = k.remote {
            remote.entries.insert(key.clone(), Entry { sha256: sha.to_string(), size: 1, owner: owner(o) });
        }
        if let Some((o, deleted)) = k.tomb {
            remote.tombstones.insert(key.clone(), Tombstone::unsigned(&owner(o), at(deleted)?));
        }
        if k.held {
            held.insert(key.clone());
        }
        if k.verified {
            verified.insert(key);
        }
    }
    let verify = |key: &str, _: &Tombstone| if verified.contains(key) { Ok(()) } else { Err("the case leaves it unverified".to_string()) };
    let merged = match merge_verified(&remote, &local, &HeldKeys::Keys(held), &owner(case.me), at(NOW)?, &verify) {
        Err(StoreError::SyncCursorConflict(_)) => return Ok("conflict".into()),
        Err(e) => bail!("the merge refused with {e:?}"),
        Ok(m) => m,
    };
    let number = |o: &str| o.strip_prefix("node-").unwrap_or("0").to_string();
    let groups: Vec<String> = (0..case.keys.len())
        .map(|i| {
            let key = path(i, case.keys[i].log);
            let entry = merged.manifest.entries.get(&key).map(|e| format!("{}:{}", e.sha256, number(&e.owner))).unwrap_or_else(|| "-".into());
            let tomb = merged
                .manifest
                .tombstones
                .get(&key)
                .map(|t| format!("{}@{}", number(&t.owner), t.deleted_at.unix_secs()))
                .unwrap_or_else(|| "-".into());
            let named = format!("`{key}`");
            let refusal = merged
                .refused
                .iter()
                .find_map(|r| match r {
                    StoreError::SyncTombstoneForeign(m) if m.contains(&named) => Some("foreign"),
                    StoreError::SyncTombstoneUnverified(m) if m.contains(&named) => Some("unverified"),
                    _ => None,
                })
                .unwrap_or("-");
            format!("{entry} {tomb} {refusal}")
        })
        .collect();
    Ok(groups.join(" ; "))
}

fn strategy() -> impl Strategy<Value = Case> {
    let entry = proptest::option::of((0u8..2, 0u8..4));
    let tomb = proptest::option::of((
        0u8..4,
        prop_oneof![Just(NOW), Just(NOW - TOMBSTONE_TTL_SECS + 1), Just(NOW - TOMBSTONE_TTL_SECS), Just(NOW - 2 * TOMBSTONE_TTL_SECS)],
    ));
    let key = (proptest::bool::weighted(0.2), any::<bool>(), any::<bool>(), entry.clone(), entry, tomb)
        .prop_map(|(log, held, verified, local, remote, tomb)| Key { log, held, verified, local, remote, tomb });
    (1u8..3, proptest::collection::vec(key, 1..5)).prop_map(|(me, keys)| Case { me, keys })
}

/// The model's merge of each case, one line per case.
fn model(reference: &Path, cases: &[Case]) -> Result<Vec<String>> {
    let mut child = Command::new(reference)
        .args(["merge", "run"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", reference.display()))?;
    {
        let stdin = child.stdin.as_mut().context("merge model stdin")?;
        for case in cases {
            writeln!(stdin, "{}", case.line())?;
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("merge model: {}", String::from_utf8_lossy(&out.stderr));
    }
    let lines: Vec<String> = String::from_utf8(out.stdout)?.lines().map(str::to_string).collect();
    if lines.len() != cases.len() {
        bail!("merge model returned {} merges for {} cases", lines.len(), cases.len());
    }
    Ok(lines)
}

/// Drop keys while the two merges still differ.
fn minimize(reference: &Path, mut case: Case) -> Result<Case> {
    let mut i = 0;
    while i < case.keys.len() && case.keys.len() > 1 {
        let mut candidate = case.clone();
        candidate.keys.remove(i);
        if model(reference, std::slice::from_ref(&candidate))?[0] != store(&candidate)? {
            case = candidate;
            i = 0;
        } else {
            i += 1;
        }
    }
    Ok(case)
}

pub fn run(args: Args) -> Result<()> {
    let reference = match args.reference {
        Some(path) => path,
        None => {
            let cwd = std::env::current_dir()?;
            let repo = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(&cwd)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
                .unwrap_or(cwd);
            let root = repo.join("formal/protocol");
            let out = Command::new("lake").arg("build").current_dir(&root).output().context("building the protocol package")?;
            if !out.status.success() {
                bail!("building the protocol package: {}", String::from_utf8_lossy(&out.stderr));
            }
            root.join(".lake/build/bin/protocol")
        }
    };
    let mut runner = TestRunner::new_with_rng(Config::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &args.seed.to_le_bytes().repeat(4)));
    let mut cases = Vec::with_capacity(args.cases);
    for _ in 0..args.cases {
        cases.push(strategy().new_tree(&mut runner).map_err(|e| anyhow::anyhow!("generating a merge case: {e}"))?.current());
    }
    let models = model(&reference, &cases)?;
    let (mut conflicts, mut refusals) = (0, 0);
    for (n, (case, expected)) in cases.into_iter().zip(models).enumerate() {
        let actual = store(&case)?;
        if expected != actual {
            let reduced = minimize(&reference, case)?;
            let model = model(&reference, std::slice::from_ref(&reduced))?.remove(0);
            bail!("MergeConformanceDrift: seed={} case={n}; case={}; model={model}; store={}", args.seed, reduced.line(), store(&reduced)?);
        }
        conflicts += usize::from(actual == "conflict");
        refusals += actual.matches("foreign").count() + actual.matches("unverified").count();
    }
    println!("merge-differential: 0 drift cases; seed={}; generated={}; conflicts={conflicts}; refusals={refusals}", args.seed, args.cases);
    Ok(())
}
