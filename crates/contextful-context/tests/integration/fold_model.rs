//! `store.fold` against the protocol package's fold model: seeded sequences of passes
//! staging, committing and dying, a successor acquiring the compaction lease, collection
//! and readers resolving the pointer, compared step by step with `protocol fold run`.

use crate::support::{decl, Fixture};
use contextful_context::fold::{collect, commit, prepare_under, staging_dir, Prepared, Staged};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::SnapshotId;
use contextful_core::time::Instant;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TABLE: &str = "events";
const SEED: u64 = 0x5eed_f01d;
const CASES: usize = 24;
const STEPS: usize = 20;

#[derive(Clone, Copy, Debug)]
enum Step {
    Acquire,
    Stage(usize),
    Commit(usize),
    Abandon(usize),
    Collect,
    Read(usize),
    Release(usize),
}

impl Step {
    fn line(self) -> String {
        match self {
            Step::Acquire => "acquire".into(),
            Step::Stage(p) => format!("stage {p}"),
            Step::Commit(p) => format!("commit {p}"),
            Step::Abandon(p) => format!("abandon {p}"),
            Step::Collect => "collect".into(),
            Step::Read(r) => format!("read {r}"),
            Step::Release(r) => format!("release {r}"),
        }
    }
}

/// A seeded sequence: stages and commits weigh most, so passes overlap and race.
fn cases() -> Vec<Vec<Step>> {
    let mut state = SEED;
    let mut draw = |n: u64| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    (0..CASES)
        .map(|_| {
            (0..STEPS)
                .map(|_| {
                    let i = (draw(2)) as usize;
                    match draw(12) {
                        0 => Step::Acquire,
                        1..=3 => Step::Stage(i),
                        4..=6 => Step::Commit(i),
                        7 => Step::Abandon(i),
                        8 | 9 => Step::Collect,
                        10 => Step::Read(i),
                        _ => Step::Release(i),
                    }
                })
                .collect()
        })
        .collect()
}

/// One pass: idle, or holding its staged snapshot and the fence it read.
enum Pass {
    Idle,
    Staged(Box<Staged>, u64),
}

struct Table {
    f: Fixture,
    d: TableDecl,
    minute: u64,
    granted: u64,
    /// Snapshot ids in staging order: the model's ids are their positions.
    ids: Vec<SnapshotId>,
    passes: [Pass; 2],
    readers: [Option<SnapshotId>; 2],
}

impl Table {
    fn now(&mut self) -> Instant {
        self.minute += 1;
        Instant::from_unix_secs(1_900_000_000 + self.minute as i64 * 60).unwrap()
    }

    fn model_id(&self, id: &SnapshotId) -> usize {
        self.ids.iter().position(|i| i == id).expect("a snapshot the case staged")
    }

    fn apply(&mut self, step: Step) {
        match step {
            Step::Acquire => self.granted += 1,
            Step::Stage(p) if matches!(self.passes[p], Pass::Idle) => {
                let landed = self.now();
                self.f.land(&self.d, &format!("run-{}", self.minute), json!([{"e": self.minute}]), &landed.to_string()).unwrap();
                let now = self.now();
                let Prepared::Staged(staged) = prepare_under(&self.f.store, &self.d, now, Some(self.granted)).unwrap() else {
                    panic!("a landed run stages")
                };
                self.ids.push(staged.manifest.snapshot_id.clone());
                self.passes[p] = Pass::Staged(staged, self.granted);
            }
            // Under the lease invariant a pass a successor superseded commits nothing.
            Step::Commit(p) if matches!(self.passes[p], Pass::Staged(_, fence) if fence == self.granted) => {
                let Pass::Staged(staged, _) = std::mem::replace(&mut self.passes[p], Pass::Idle) else { unreachable!() };
                commit(&self.f.store, *staged).unwrap();
            }
            Step::Abandon(p) => self.passes[p] = Pass::Idle,
            Step::Collect => {
                let now = self.now();
                collect(&self.f.store, &self.d, now).unwrap();
            }
            Step::Read(r) => self.readers[r] = self.f.store.pointer(TABLE).unwrap().map(|(p, _)| p.snapshot_id),
            Step::Release(r) => self.readers[r] = None,
            _ => {}
        }
    }

    /// The state in the model executable's projection.
    fn show(&self) -> String {
        let pointer = self.f.store.pointer(TABLE).unwrap();
        let ptr = pointer.as_ref().map(|(p, _)| self.model_id(&p.snapshot_id).to_string()).unwrap_or_else(|| "-".into());
        let fence = pointer.as_ref().and_then(|(p, _)| p.fence).map(|f| f.to_string()).unwrap_or_else(|| "-".into());
        let chain = if pointer.is_some() { self.f.store.chain(TABLE).unwrap().0 } else { Vec::new() };
        let chain: Vec<String> = chain.iter().map(|s| self.model_id(&s.snapshot_id).to_string()).collect();
        let dirs: Vec<String> = self
            .ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let published = self.f.store.snapshot_dir(TABLE, id).unwrap().is_dir();
                let staging = staging_dir(&self.f.store, TABLE, id).unwrap().is_dir();
                let d = format!("{}{}", if published { "f" } else { "" }, if staging { "s" } else { "" });
                format!("{i}:{}", if d.is_empty() { "-" } else { &d })
            })
            .collect();
        let readers: Vec<String> = self
            .readers
            .iter()
            .enumerate()
            .map(|(r, id)| format!("r{r}={}", id.as_ref().map(|id| self.model_id(id).to_string()).unwrap_or_else(|| "-".into())))
            .collect();
        format!("ptr={ptr} fence={fence} chain=[{}] dirs=[{}] {}", chain.join(","), dirs.join(" "), readers.join(" "))
    }
}

fn model(reference: &Path, steps: &[Step]) -> Vec<String> {
    let mut child = Command::new(reference).args(["fold", "run"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for step in steps {
            writeln!(stdin, "{}", step.line()).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().lines().map(str::to_string).collect()
}

fn reference() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../formal/protocol");
    match Command::new("lake").arg("build").current_dir(&root).output() {
        Ok(out) if out.status.success() => Some(root.join(".lake/build/bin/protocol")),
        Ok(out) => panic!("building the protocol package: {}", String::from_utf8_lossy(&out.stderr)),
        Err(_) => {
            assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "Lean is required");
            None
        }
    }
}

/// A pass publishes by replacing `_pointer.json` conditioned on the ETag read at pass start. Object stores use `If-Match`; filesystems use a version-checked rename.
// spec: store.fold.pointer-commit@404387b7
// spec: store.fold.lost-pointer@e9fa3f89
// spec: store.fold.staging-collected@8832e10f
// spec: store.fold.supersedes@97cbcf3a
// spec: store.fold.non-blocking@05f96f28
// spec: store.fold.compaction-lease@6b473e0b
// spec: assurance.model.fold-model@028cd636
// spec: assurance.differential-test.fold-harness@961390ae
#[test]
fn the_store_and_the_fold_model_agree_after_every_step() {
    let Some(reference) = reference() else { return };
    let mut steps_run = 0;
    for (n, steps) in cases().into_iter().enumerate() {
        let expected = model(&reference, &steps);
        let mut t = Table {
            f: Fixture::new(),
            d: decl("name = \"events\"\nwrite_mode = \"replace\""),
            minute: 0,
            granted: 1,
            ids: Vec::new(),
            passes: [Pass::Idle, Pass::Idle],
            readers: [None, None],
        };
        for (i, step) in steps.iter().enumerate() {
            t.apply(*step);
            let actual = t.show();
            let lines: Vec<String> = steps[..=i].iter().map(|s| s.line()).collect();
            assert_eq!(actual, expected[i], "case {n} step {i}: {lines:?}");
            steps_run += 1;
        }
    }
    assert_eq!(steps_run, CASES * STEPS);
}
