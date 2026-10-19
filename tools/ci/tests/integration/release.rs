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

/// A `spec/status.md` whose milestone table carries `header` and `rows` verbatim.
fn status_table(header: &str, rows: &[&str]) -> String {
    let columns = header.trim_matches('|').split('|').count();
    let mut s = format!("# Status\n\n## Milestones\n\n{header}\n|{}\n", " --- |".repeat(columns));
    rows.iter().for_each(|r| s.push_str(&format!("{r}\n")));
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
const FAKE_GPG: &str = "#!/bin/sh\ncat >/dev/null\nprintf '\\n[GNUPG:] SIG_CREATED D 1 8 00 0 0\\n' >&2\n\
printf '%s\\n' '-----BEGIN PGP SIGNATURE-----' '' 'ZmFrZQ==' '-----END PGP SIGNATURE-----'\n";

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

    /// Commit `format` as the store format constant's `{ major, minor }` fields, in a commit
    /// of its own so the bump that follows changes no source.
    fn store_format(&self, format: &str) {
        self.store_format_source(&format!("impl FormatVersion {{\n    pub const CURRENT: FormatVersion = FormatVersion {{ {format} }};\n}}\n"));
    }

    fn store_format_source(&self, source: &str) {
        self.repo.write("crates/contextful-core/Cargo.toml", &manifest("contextful-core", ""));
        self.repo.write("crates/contextful-core/src/lib.rs", "");
        self.repo.write("crates/contextful-core/src/store/lay_out.rs", source);
        self.repo.lock();
        self.repo.commit("store format");
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

/// A release tag is `v0.<minor>.<patch>`: `<minor>` counts the milestones computing `closed` in the tagged commit's `spec/status.md` plus each store format major past `1`, and `<patch>` counts the existing tags `v0.<minor>.*`.
// spec: assurance.release.version@7a27ee41
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

/// A commit's store format is the `FormatVersion::CURRENT` constant in its `crates/contextful-core/src/store/lay_out.rs`, and `1.0` where that constant is absent; a constant that does not parse raises `TagStoreFormatUnread`.
// spec: assurance.release.store-format@5ede2a87
#[test]
fn a_store_format_major_past_one_raises_the_minor() {
    // No constant reads as `1.0`, which adds nothing to the closed-milestone count.
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0");

    // A `1.x` constant still adds nothing; the next tag is a patch.
    r.store_format("major: 1, minor: 4");
    r.bump(&THREE_OF_FOUR, "0.3.1");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0\nv0.3.1");

    // Major 2 with the same three milestones closed computes the next minor.
    r.store_format("major: 2, minor: 0");
    r.bump(&THREE_OF_FOUR, "0.3.2");
    let err = refused(&r.tag(&[]), "TagWorkspaceVersionMismatch");
    assert!(err.contains("0.4.0"), "{err}");
    r.bump(&THREE_OF_FOUR, "0.4.0");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0\nv0.3.1\nv0.4.0");

    // A constant whose fields do not parse is refused before the gate.
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    r.store_format("major: two, minor: 0");
    let err = refused(&r.tag(&[]), "TagStoreFormatUnread");
    assert!(err.contains("lay_out.rs"), "{err}");
    assert!(!r.cargo_ran(), "the gate ran over an unread store format");
    assert_eq!(r.tags(), "");
}

/// A tag whose store format major exceeds the highest existing tag's carries the message line `Store format <old> → <new>: upgrade every reader, then every writer together.`
// spec: assurance.release.format-notes@2eef30a7
#[test]
fn a_tag_raising_the_store_format_major_names_the_upgrade_order() {
    let line = "Store format 1.0 → 2.0: upgrade every reader, then every writer together.";
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.1"));
    r.earlier("v0.3.0");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!r.repo.git(&["cat-file", "tag", "v0.3.1"]).contains("Store format"), "an unchanged format names an upgrade");

    r.store_format("major: 2, minor: 0");
    r.bump(&THREE_OF_FOUR, "0.4.0");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let object = r.repo.git(&["cat-file", "tag", "v0.4.0"]);
    assert!(object.contains(line), "the message omits the upgrade order: {object}");

    // A minor step within major 2 names no upgrade.
    r.store_format("major: 2, minor: 1");
    r.bump(&THREE_OF_FOUR, "0.4.1");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!r.repo.git(&["cat-file", "tag", "v0.4.1"]).contains("Store format"), "a minor format step names an upgrade");
}

/// A computed `<minor>` equal to the highest existing tag's, while `HEAD`'s store format major differs from that tag's, raises `TagFormatBreakPatch`, naming both formats.
// spec: assurance.release.format-break-patch@1b146c18
#[test]
fn a_format_break_under_an_unchanged_minor_is_refused() {
    // Milestone 5 reopens as the format moves to major 2: 2 closed plus 1 keeps minor 3.
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.1"));
    r.earlier("v0.3.0");
    let mut two = THREE_OF_FOUR;
    two[3].2 = "open";
    r.store_format("major: 2, minor: 0");
    r.bump(&two, "0.3.1");
    let err = refused(&r.tag(&[]), "TagFormatBreakPatch");
    assert!(err.contains("1.0") && err.contains("2.0") && err.contains("v0.3.0"), "{err}");
    assert!(!r.cargo_ran(), "the gate ran for a format break under a patch");
    assert_eq!(r.tags(), "v0.3.0");
}

#[test]
fn a_structural_store_format_constant_raises_the_minor_and_names_the_upgrade() {
    for source in [
        "impl FormatVersion { pub const CURRENT: Self = Self { major: 2, minor: 0 }; }",
        "impl FormatVersion { pub const CURRENT : FormatVersion = FormatVersion {\n minor: 0,\n major: 2,\n }; }",
        "impl FormatVersion { pub const CURRENT:\nSelf\n=\nFormatVersion { major: 2, minor: 0 }; }",
        "impl self::FormatVersion { pub const CURRENT: Self = Self { major: 2, minor: 0 }; }",
        "impl FormatVersion { pub const CURRENT: self::FormatVersion = self::FormatVersion { major: 2, minor: 0 }; }",
    ] {
        let r = Release::new(&THREE_OF_FOUR, Some("0.3.1"));
        r.earlier("v0.3.0");
        r.store_format_source(source);
        let err = refused(&r.tag(&[]), "TagWorkspaceVersionMismatch");
        assert!(err.contains("0.4.0"), "{source}: {err}");
        assert!(!r.cargo_ran(), "the gate ran under a mismatched version");

        r.bump(&THREE_OF_FOUR, "0.4.0");
        let o = r.tag(&[]);
        assert!(o.status.success(), "{source}: {}", stderr(&o));
        let message = r.repo.git(&["cat-file", "tag", "v0.4.0"]);
        assert!(message.contains("Store format 1.0 → 2.0: upgrade every reader, then every writer together."), "{message}");
    }
}

#[test]
fn a_structural_store_format_constant_cannot_hide_a_format_break_in_a_patch() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    r.store_format_source("impl FormatVersion { pub const CURRENT: Self = Self { major: 1, minor: 4 }; }");
    r.bump(&THREE_OF_FOUR, "0.3.1");
    r.earlier("v0.3.0");
    r.store_format_source("impl FormatVersion { pub const CURRENT: Self = Self { major: 2, minor: 0 }; }");
    let mut two = THREE_OF_FOUR;
    two[3].2 = "open";
    r.bump(&two, "0.3.1");
    let err = refused(&r.tag(&[]), "TagFormatBreakPatch");
    assert!(err.contains("1.4") && err.contains("2.0"), "{err}");
    assert!(!r.cargo_ran(), "the gate ran for a format break under a patch");
    assert_eq!(r.tags(), "v0.3.0");
}

#[test]
fn a_present_unreadable_store_format_is_refused_before_the_gate() {
    for source in [
        "impl FormatVersion { pub const CURRENT: Self = Self::new(2, 0); }",
        "impl self::FormatVersion { pub const CURRENT: Self = Self::new(2, 0); }",
        "impl FormatVersion { pub const CURRENT: Self = Self { major: 2 + 0, minor: 0 }; }",
        "impl FormatVersion { pub const CURRENT: Self = Self { major: 2 }; }",
        "impl FormatVersion { pub const CURRENT: Self = Self { major: 1, major: 2, minor: 0 }; }",
        "impl FormatVersion { pub const CURRENT: Self = Other { major: 2, minor: 0 }; }",
        "impl FormatVersion { pub const CURRENT: u64 = 2; }",
        "impl FormatVersion { pub const CURRENT: Self = Self { major: 2, minor: 0 }; pub const CURRENT: Self = Self { major: 3, minor: 0 }; }",
        "impl FormatVersion { pub const CURRENT: Self = Self { major: 2, minor: }; }",
    ] {
        let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
        r.store_format_source(source);
        let err = refused(&r.tag(&[]), "TagStoreFormatUnread");
        assert!(err.contains("lay_out.rs"), "{err}");
        assert!(!r.cargo_ran(), "the gate ran over {source}");
        assert_eq!(r.tags(), "");
    }
}

#[test]
fn a_file_without_the_format_version_current_constant_keeps_the_historical_format() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.2.0"));
    r.store_format_source("impl Other {\n    pub const CURRENT: FormatVersion = FormatVersion { major: 9, minor: 0 };\n}\nimpl FormatVersion { pub const OLD: Self = Self { major: 8, minor: 0 }; }");
    r.bump(&THREE_OF_FOUR, "0.3.0");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.3.0");
}

/// The `Closed` column is found by its header name wherever it sits, and a table without one
/// closes no milestone.
#[test]
fn the_closed_column_is_found_by_name_and_its_absence_closes_nothing() {
    // `Closed` precedes a trailing column: the last cell of every row reads `closed`, yet
    // only the rows whose `Closed` cell reads `closed` count.
    let r = Release::new(&THREE_OF_FOUR, Some("0.2.0"));
    r.repo.write(
        "spec/status.md",
        &status_table(
            "| Milestone | Closed | Note |",
            &["| 0 — The test-first gate | closed | closed |", "| 1 — The authority core | open | closed |", "| 2 — The store | closed | closed |"],
        ),
    );
    r.repo.commit("closed column moved");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.2.0");

    // No `Closed` column: a passing acceptance test closes nothing, so the minor is 0.
    let r = Release::new(&THREE_OF_FOUR, Some("0.0.0"));
    r.repo.write(
        "spec/status.md",
        &status_table("| Milestone | Acceptance |", &["| 0 — The test-first gate | passing |", "| 2 — The store | passing |"]),
    );
    r.repo.commit("no closed column");
    let o = r.tag(&[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(r.tags(), "v0.0.0");
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
    assert!(err.contains("`cargo fetch --locked` exited 101"), "the refusal names no failing stage command: {err}");
    assert!(r.cargo_ran());
    assert_eq!(r.tags(), "");
}

impl Release {
    fn release(&self) -> Output {
        let path = format!("{}:{}", self.bin.path().display(), std::env::var("PATH").unwrap_or_default());
        Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
            .args(["release", "--target", "x86_64-unknown-linux-musl", "--target-dir", "target", "--out", "dist"])
            .current_dir(&self.repo.root)
            .env_remove("CARGO_TARGET_DIR")
            .env("PATH", path)
            .output()
            .unwrap()
    }
}

/// `contextful-ci release` over a `HEAD` carrying a `v*` tag, none of which equals `v` plus the `[workspace.package]` version, raises `ReleaseTagVersionMismatch`, naming both, before any build.
// spec: assurance.release.release-tag-version@c43a6156
#[test]
fn a_release_over_a_tag_other_than_the_workspace_version_is_refused() {
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    r.repo.git(&["tag", "-a", "-m", "v0.3.2", "v0.3.2"]);
    let o = r.release();
    let err = stderr(&o);
    assert!(!o.status.success() && err.contains("ReleaseTagVersionMismatch"), "expected ReleaseTagVersionMismatch: {err}");
    assert!(err.contains("v0.3.2") && err.contains("0.3.0"), "{err}");
    assert!(!r.cargo_ran(), "a build ran under a mismatched tag");

    // A matching tag beside the stray one clears the check, and the build starts.
    r.repo.git(&["tag", "-a", "-m", "v0.3.0", "v0.3.0"]);
    let o = r.release();
    assert!(!stderr(&o).contains("ReleaseTagVersionMismatch"), "{}", stderr(&o));
    assert!(r.cargo_ran(), "the build did not start under a matching tag");

    // An untagged head builds as before.
    let r = Release::new(&THREE_OF_FOUR, Some("0.3.0"));
    let o = r.release();
    assert!(!stderr(&o).contains("ReleaseTagVersionMismatch"), "{}", stderr(&o));
    assert!(r.cargo_ran(), "the build did not start on an untagged head");
}
