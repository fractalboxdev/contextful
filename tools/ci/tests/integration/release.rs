//! `assurance.release`: `contextful-ci tag` against scratch repositories whose
//! `spec/status.md` carries a milestone table, with `cargo` answered by a script that hands
//! `metadata` and `tree` to the real cargo and records every other invocation.

use crate::{manifest, stderr, Repo};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A milestone row: number, name, closed verdict. Every row's acceptance test computes
/// `passing`, so only the `Closed` column separates a counted milestone from an open one.
type Row = (u32, &'static str, &'static str);

const THREE_OF_FOUR: [Row; 4] = [
    (0, "The test-first gate", "closed"),
    (1, "The authority core", "open"),
    (2, "The store", "closed"),
    (5, "The read face", "closed"),
];

fn status(rows: &[Row]) -> String {
    let mut s = String::from(
        "# Status\n\n## Milestones\n\n| Milestone | Operations | Clauses | Performed | Acceptance | Closed |\n| --- | --- | --- | --- | --- | --- |\n",
    );
    for (n, name, closed) in rows {
        s.push_str(&format!("| {n} — {name} | 1 | 4 | 2 | passing | {closed} |\n"));
    }
    s.push_str("\nUnscheduled operations: 0.\n");
    s
}

fn root_manifest(version: Option<&str>) -> String {
    let mut s = String::from("[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]\n\n[workspace.package]\nlicense = \"Apache-2.0\"\n");
    if let Some(v) = version {
        s.push_str(&format!("version = \"{v}\"\n"));
    }
    s
}

/// A `gpg` that signs anything: it drains the payload, reports the signature to git on
/// the status descriptor and prints an armoured block.
const FAKE_GPG: &str = "#!/bin/sh\ncat >/dev/null\necho '[GNUPG:] SIG_CREATED D 1 8 00 0 0' >&2\n\
printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nZmFrZQ==\\n-----END PGP SIGNATURE-----\\n'\n";

struct Release {
    repo: Repo,
    bin: tempfile::TempDir,
}

impl Release {
    /// A repository on `main` whose head carries `rows` in `spec/status.md` and `version` in
    /// `[workspace.package]`, an acceptance package, and a signing identity.
    fn new(rows: &[Row], version: Option<&str>) -> Release {
        let repo = Repo::init();
        repo.write("Cargo.toml", &root_manifest(version));
        repo.write("crates/acceptance/Cargo.toml", &manifest("contextful-acceptance", ""));
        repo.write("crates/acceptance/src/lib.rs", "");
        repo.write("crates/acceptance/tests/integration/main.rs", "");
        repo.write("spec/status.md", &status(rows));
        repo.lock();
        repo.commit("release fixture");
        let bin = tempfile::tempdir().unwrap();
        let gpg = bin.path().join("fake-gpg");
        executable(&gpg, FAKE_GPG);
        for (k, v) in [
            ("user.name", "t"),
            ("user.email", "t@example.com"),
            ("user.signingkey", "t@example.com"),
            ("gpg.format", "openpgp"),
            ("gpg.program", gpg.to_str().unwrap()),
        ] {
            repo.git(&["config", k, v]);
        }
        let r = Release { repo, bin };
        r.cargo_exits(0);
        r
    }

    /// Answer every `cargo` invocation other than `metadata` and `tree` with `code`, recording it.
    fn cargo_exits(&self, code: i32) {
        let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in metadata|tree) exec '{real}' \"$@\" ;; esac\necho \"$*\" >> '{}'\nexit {code}\n",
            self.ran().display()
        );
        executable(&self.bin.path().join("cargo"), &script);
    }

    fn ran(&self) -> PathBuf {
        self.bin.path().join("cargo-ran")
    }

    fn cargo_ran(&self) -> bool {
        self.ran().exists()
    }

    fn tag(&self, args: &[&str]) -> Output {
        let path = format!("{}:{}", self.bin.path().display(), std::env::var("PATH").unwrap_or_default());
        Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
            .args(["tag", "--branch", "main"])
            .args(args)
            .current_dir(&self.repo.root)
            .env_remove("CARGO_TARGET_DIR")
            .env("PATH", path)
            .output()
            .unwrap()
    }

    fn tags(&self) -> String {
        self.repo.git(&["tag", "--list"])
    }

    /// An unsigned annotated tag on `HEAD~1`, standing for an earlier release.
    fn earlier(&self, name: &str) {
        self.repo.git(&["tag", "-a", "-m", name, name, "HEAD~1"]);
    }

    /// Commit `rows` and `version` as the head, the bump commit a tag follows.
    fn bump(&self, rows: &[Row], version: &str) {
        self.repo.write("spec/status.md", &status(rows));
        self.repo.write("Cargo.toml", &root_manifest(Some(version)));
        self.repo.commit("bump");
    }
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn refused(o: &Output, code: &str) -> String {
    let err = stderr(o);
    assert!(!o.status.success(), "tag succeeded: {err}");
    assert!(err.contains(code), "expected {code}: {err}");
    err
}

/// A release tag is `v0.<closed>.<patch>`: `<closed>` counts the milestones computing `closed` in the tagged commit's `spec/status.md`, and `<patch>` counts the existing tags `v0.<closed>.*`.
// spec: assurance.release.version@63e7cc56
#[test]
fn the_version_counts_closed_milestones_and_earlier_tags_of_that_count() {
    // Milestones 0, 2 and 5 close and 1 is open though its acceptance test passes: a count,
    // not the highest number.
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0");

    // A second tag with the same count of closed milestones raises the patch.
    r.bump(&THREE_OF_FOUR, "0.3.1");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0\nv0.3.1");

    // A milestone closing raises the minor and restarts the patch.
    let mut four = THREE_OF_FOUR;
    four[1].2 = "closed";
    r.bump(&four, "0.4.0");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0\nv0.3.1\nv0.4.0");
}

/// `contextful-ci tag` creates a signed annotated tag on `HEAD` naming the closed milestones once every refusal of this operation clears, and never moves or replaces an existing tag.
// spec: assurance.release.annotated@f88b3e0a
#[test]
fn the_tag_is_signed_annotated_on_head_and_leaves_earlier_tags_in_place() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.1"));
    r.earlier("v0.3.0");
    let earlier = r.repo.git(&["rev-parse", "v0.3.0"]);

    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(r.cargo_ran(), "the gate ran no stage before tagging");
    assert_eq!(r.repo.git(&["cat-file", "-t", "v0.3.1"]), "tag", "the tag is lightweight");
    assert_eq!(r.repo.git(&["rev-parse", "v0.3.1^{commit}"]), r.repo.head());
    let object = r.repo.git(&["cat-file", "tag", "v0.3.1"]);
    assert!(object.contains("-----BEGIN PGP SIGNATURE-----"), "unsigned: {object}");
    for closed in ["0 — The test-first gate", "2 — The store", "5 — The read face"] {
        assert!(object.contains(closed), "the message omits `{closed}`: {object}");
    }
    assert!(!object.contains("The authority core"), "the message names an open milestone: {object}");
    assert_eq!(r.repo.git(&["rev-parse", "v0.3.0"]), earlier, "an earlier tag moved");
}

/// A working tree or index differing from `HEAD` raises `TagTreeDirty`.
// spec: assurance.release.dirty-tree@0e8eacd7
#[test]
fn a_tree_differing_from_head_is_refused_before_the_gate() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    r.repo.write("crates/demo/src/lib.rs", "pub fn double(x: i32) -> i32 {\n    x + x\n}\n");
    refused(&r.tag(&[]), "TagTreeDirty");

    r.repo.git(&["add", "-A"]);
    refused(&r.tag(&[]), "TagTreeDirty");

    r.repo.git(&["reset", "-q", "--hard"]);
    r.repo.write("crates/demo/tests/integration/extra.rs", "");
    refused(&r.tag(&[]), "TagTreeDirty");

    assert!(!r.cargo_ran(), "the gate ran over a dirty tree");
    assert_eq!(r.tags(), "");
}

/// A `HEAD` unreachable from the default branch, `origin/HEAD` unless `--branch` names another, raises `TagOffDefaultBranch`.
// spec: assurance.release.off-branch@6d701d97
#[test]
fn a_head_the_default_branch_does_not_reach_is_refused() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    r.repo.git(&["checkout", "-q", "-b", "side"]);
    r.repo.write("notes.txt", "off the default branch\n");
    r.repo.commit("side");
    let err = refused(&r.tag(&[]), "TagOffDefaultBranch");
    assert!(err.contains("main"), "{err}");
    assert!(!r.cargo_ran(), "the gate ran off the default branch");
    assert_eq!(r.tags(), "");

    // A commit the default branch reaches passes the check even when checked out detached.
    r.repo.git(&["checkout", "-q", "--detach", "main"]);
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0");
}

/// A computed version below the highest existing `v<major>.<minor>.<patch>` tag raises `TagVersionRegressed`, naming both.
// spec: assurance.release.version-regressed@bc57e7b3
#[test]
fn a_milestone_reopening_below_the_last_tag_is_refused() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.1"));
    r.earlier("v0.3.0");
    let mut two = THREE_OF_FOUR;
    two[3].2 = "open";
    r.bump(&two, "0.2.0");
    let err = refused(&r.tag(&[]), "TagVersionRegressed");
    assert!(err.contains("v0.2.0") && err.contains("v0.3.0"), "{err}");
    assert!(!r.cargo_ran(), "the gate ran for a regressed version");
    assert_eq!(r.tags(), "v0.3.0");
}

/// A `[workspace.package]` version in `HEAD`'s `Cargo.toml` that is absent or differs from the computed version raises `TagWorkspaceVersionMismatch`, naming both.
// spec: assurance.release.workspace-version@45d3121e
#[test]
fn a_workspace_version_other_than_the_computed_one_is_refused() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.1.0"));
    let err = refused(&r.tag(&[]), "TagWorkspaceVersionMismatch");
    assert!(err.contains("0.1.0") && err.contains("0.3.0"), "{err}");

    let r = Release::new(&THREE_OF_FOUR, None);
    let err = refused(&r.tag(&[]), "TagWorkspaceVersionMismatch");
    assert!(err.contains("0.3.0"), "{err}");
    assert!(!r.cargo_ran(), "the gate ran under a mismatched workspace version");
    assert_eq!(r.tags(), "");
}

/// Once the other refusals clear, every gate stage runs against `HEAD` with `HEAD~1` as base; a failing stage raises `TagGateFailed`, naming the stage's refusal, and no tag is created.
// spec: assurance.release.gate-failed@8157b074
#[test]
fn a_failing_gate_stage_is_refused_and_creates_no_tag() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    r.cargo_exits(101);
    let err = refused(&r.tag(&[]), "TagGateFailed");
    assert!(err.contains("contextful-spec"), "the refusal names no failing stage command: {err}");
    assert!(r.cargo_ran());
    assert_eq!(r.tags(), "");
}
