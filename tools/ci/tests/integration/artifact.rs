//! `assurance.build.targets` and `assurance.build.release-artifact`: `contextful-ci release`
//! over this workspace, with `cargo build` answered by a script that writes a stand-in
//! binary and every `cargo tree` resolved by the real cargo.

use crate::repo_root;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Command, Output};

const LINUX_X86: &str = "x86_64-unknown-linux-musl";

fn ci(args: &[&str], bin: Option<&Path>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful-ci"));
    c.args(args).current_dir(repo_root());
    if let Some(bin) = bin {
        c.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    }
    c.output().unwrap()
}

/// A `cargo` whose `build` writes `contextful` under `$CARGO_TARGET_DIR/<target>/release`
/// holding its own arguments, and which hands every other subcommand to the real cargo.
fn fake_cargo(dir: &Path) {
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = build ]; then\n  t=''; prev=''\n  for a in \"$@\"; do [ \"$prev\" = --target ] && t=\"$a\"; prev=\"$a\"; done\n  \
         mkdir -p \"$CARGO_TARGET_DIR/$t/release\"\n  echo \"$*\" > \"$CARGO_TARGET_DIR/$t/release/contextful\"\n  exit 0\nfi\nexec '{real}' \"$@\"\n"
    );
    let path = dir.join("cargo");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn version() -> String {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    doc["workspace"]["package"]["version"].as_str().unwrap().to_string()
}

fn sbom_names(path: &Path) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(v["bomFormat"], "CycloneDX");
    assert_eq!(v["specVersion"], "1.5");
    v["components"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect()
}

/// All three profiles cross-compile to `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`; edge and full also build for `aarch64-apple-darwin` and `x86_64-apple-darwin`.
// spec: assurance.build.targets@6f74a943
#[test]
fn the_release_matrix_is_every_profile_on_musl_and_edge_and_full_on_darwin() {
    let out = ci(&["release", "--plan"], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut cells: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    cells.sort();
    let mut expected = Vec::new();
    for p in ["contextful-control", "contextful-edge", "contextful-full"] {
        for t in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
            expected.push(format!("{p} {t}"));
        }
    }
    for p in ["contextful-edge", "contextful-full"] {
        for t in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            expected.push(format!("{p} {t}"));
        }
    }
    expected.sort();
    assert_eq!(cells, expected);

    let wasi = ci(&["release", "--target", "wasm32-wasip2", "--plan"], None);
    assert!(!wasi.status.success(), "a wasm32-wasip2 release target is accepted");
}

/// Each profile ships a release archive with a SHA-256 checksum and an SBOM, a package-manager formula and an independently tagged container image; the bare formula name and the install script resolve to the full profile.
// spec: assurance.build.release-artifact@54ae343d
#[test]
fn a_dry_run_release_packages_three_archives_with_checksums_sboms_and_formulae() {
    let bin = tempfile::tempdir().unwrap();
    fake_cargo(bin.path());
    let target = tempfile::tempdir().unwrap();
    let dist = tempfile::tempdir().unwrap();
    let td = target.path().to_str().unwrap();
    let out = dist.path().to_str().unwrap();
    let v = version();

    let run = ci(&["release", "--target", LINUX_X86, "--target-dir", td, "--out", out], Some(bin.path()));
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));

    for short in ["control", "edge", "full"] {
        let stem = format!("contextful-{short}-{v}-{LINUX_X86}");
        let archive = dist.path().join(format!("{stem}.tar.gz"));
        let bytes = std::fs::read(&archive).unwrap_or_else(|e| panic!("{}: {e}", archive.display()));
        let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        let sum = std::fs::read_to_string(dist.path().join(format!("{stem}.tar.gz.sha256"))).unwrap();
        assert_eq!(sum, format!("{digest}  {stem}.tar.gz\n"));

        let listed = Command::new("tar").args(["-tzf"]).arg(&archive).output().unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert!(listed.contains(&format!("{stem}/contextful")) && listed.contains(&format!("{stem}/LICENSE")), "{listed}");
        let built = Command::new("tar").args(["-xzOf"]).arg(&archive).arg(format!("{stem}/contextful")).output().unwrap();
        let built = String::from_utf8_lossy(&built.stdout);
        assert!(built.contains(&format!("--features contextful-{short} --target {LINUX_X86}")), "{built}");
        assert!(built.contains("--no-default-features") && built.contains("--release"), "{built}");
    }

    let control = sbom_names(&dist.path().join(format!("contextful-control-{v}-{LINUX_X86}.cdx.json")));
    let edge = sbom_names(&dist.path().join(format!("contextful-edge-{v}-{LINUX_X86}.cdx.json")));
    let full = sbom_names(&dist.path().join(format!("contextful-full-{v}-{LINUX_X86}.cdx.json")));
    assert!(control.iter().any(|n| n == "contextful-core"), "{control:?}");
    assert!(!control.iter().any(|n| n == "duckdb"), "the control SBOM lists the SQL engine");
    assert!(edge.iter().any(|n| n == "duckdb") && !edge.iter().any(|n| n == "contextful-engine"), "{edge:?}");
    assert!(full.iter().any(|n| n == "contextful-engine") && full.iter().any(|n| n == "duckdb"), "{full:?}");

    // The other targets, so every formula finds each archive it names.
    for t in ["aarch64-unknown-linux-musl", "aarch64-apple-darwin", "x86_64-apple-darwin"] {
        let run = ci(&["release", "--target", t, "--target-dir", td, "--out", out], Some(bin.path()));
        assert!(run.status.success(), "{t}: {}", String::from_utf8_lossy(&run.stderr));
    }
    let formula = ci(&["formula", "--dist", out, "--base-url", "https://example.com/v"], None);
    assert!(formula.status.success(), "{}", String::from_utf8_lossy(&formula.stderr));
    let bare = std::fs::read_to_string(dist.path().join("Formula/contextful.rb")).unwrap();
    let full_rb = std::fs::read_to_string(dist.path().join("Formula/contextful-full.rb")).unwrap();
    assert!(bare.contains("class Contextful < Formula"), "{bare}");
    assert_eq!(bare.replace("class Contextful <", "class ContextfulFull <"), full_rb, "the bare formula installs another profile");
    assert!(bare.contains(&format!("https://example.com/v/contextful-full-{v}-aarch64-apple-darwin.tar.gz")), "{bare}");
    let control_rb = std::fs::read_to_string(dist.path().join("Formula/contextful-control.rb")).unwrap();
    assert!(!control_rb.contains("on_macos"), "{control_rb}");
    let sums = std::fs::read_to_string(dist.path().join("SHA256SUMS")).unwrap();
    assert_eq!(sums.lines().count(), 10, "{sums}");

    let install = std::fs::read_to_string(repo_root().join("install.sh")).unwrap();
    assert!(install.contains("profile=\"${CONTEXTFUL_PROFILE:-contextful-full}\""), "the install script defaults to another profile");
}

/// The release workflow builds every target of the matrix and an image per profile, and a
/// dry run publishes nothing.
#[test]
fn the_release_workflow_builds_every_matrix_target_and_publishes_only_off_a_dry_run() {
    let out = ci(&["release", "--plan"], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let plan = String::from_utf8_lossy(&out.stdout).to_string();
    let mut targets: Vec<&str> = plan.lines().filter_map(|l| l.split_whitespace().nth(1)).collect();
    targets.sort();
    targets.dedup();
    let mut profiles: Vec<&str> = plan.lines().filter_map(|l| l.split_whitespace().next()).collect();
    profiles.dedup();

    let yml = std::fs::read_to_string(repo_root().join(".github/workflows/release.yml")).unwrap();
    let mut built: Vec<&str> = yml.lines().filter_map(|l| l.trim().strip_prefix("- target: ")).collect();
    built.sort();
    assert_eq!(built, targets, "the workflow's build matrix differs from the release matrix");
    // Each target builds natively on a hosted runner of its own architecture; a retired
    // image schedules no job, and its archives then never reach the formula step.
    let lines: Vec<&str> = yml.lines().map(str::trim).collect();
    let runner_of = |target: &str| {
        let at = lines.iter().position(|l| *l == format!("- target: {target}")).unwrap();
        lines[at + 1].strip_prefix("runner: ").unwrap_or_else(|| panic!("`{target}` names no runner")).to_string()
    };
    for (target, runner) in [
        ("x86_64-unknown-linux-musl", "ubuntu-24.04"),
        ("aarch64-unknown-linux-musl", "ubuntu-24.04-arm"),
        ("aarch64-apple-darwin", "macos-15"),
        ("x86_64-apple-darwin", "macos-15-intel"),
    ] {
        assert_eq!(runner_of(target), runner, "`{target}` runs on another runner");
    }
    let images = yml.lines().find_map(|l| l.trim().strip_prefix("profile: [")).and_then(|l| l.strip_suffix(']')).expect("an image matrix");
    let images: Vec<&str> = images.split(',').map(str::trim).collect();
    assert_eq!(images, profiles);
    assert!(yml.contains("release --target ${{ matrix.target }} --out dist"), "{yml}");
    assert!(yml.contains("formula --dist dist"), "{yml}");
    for publish in ["gh release create", "push: ${{ env.DRY_RUN != 'true' }}"] {
        let at = yml.find(publish).unwrap_or_else(|| panic!("no `{publish}` step"));
        let before = &yml[..at];
        assert!(publish.starts_with("push") || before.rfind("if: env.DRY_RUN != 'true'") > before.rfind("- uses:").max(before.rfind("- run:")), "`{publish}` runs on a dry run");
    }
}

/// No profile ships a `wasm32-wasip2` release; a scheduled-tier ledger entry builds the edge profile for it and holds the compressed artifact to {{assurance.gate.edge-budget}}.
///
/// Under `contextful-ci measure --tier scheduled` the probe runs `contextful-ci wasi-probe`,
/// which records the compressed size or, when the build fails, nothing; elsewhere it checks
/// the ledger schedules it and the release matrix leaves the target out.
// spec: assurance.build.wasi-probe@67a91a2c
#[test]
fn the_edge_profile_probes_wasm32_wasip2_against_its_footprint_budget() {
    let root = repo_root();
    let text = std::fs::read_to_string(root.join("evals/ledger.toml")).unwrap();
    let ledger: toml::Value = toml::from_str(&text).unwrap();
    let entry = &ledger["entry"]["edge-wasip2-footprint"];
    assert_eq!(entry["tier"].as_str(), Some("scheduled"));
    assert_eq!(entry["clause"].as_str(), Some("assurance.build.wasi-probe"));
    let plan = ci(&["release", "--plan"], None);
    assert!(!String::from_utf8_lossy(&plan.stdout).contains("wasm32"), "a release ships for wasm32");

    if std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_none() {
        return;
    }
    let probe = ci(&["wasi-probe"], None);
    assert!(probe.status.success(), "{}", String::from_utf8_lossy(&probe.stderr));
}

/// A `cargo` whose `build` exits `code`, first writing `bytes` as the wasm artifact under
/// `$CARGO_TARGET_DIR` when `code` is 0.
fn wasi_cargo(dir: &Path, code: i32, bytes: usize) {
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = build ] || exit 2\n[ {code} = 0 ] || {{ echo 'error: no available targets' >&2; exit {code}; }}\n\
         d=\"$CARGO_TARGET_DIR/wasm32-wasip2/release\"\nmkdir -p \"$d\"\nhead -c {bytes} /dev/zero > \"$d/contextful.wasm\"\n"
    );
    let path = dir.join("cargo");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn probe(bin: &Path, records: &Path, target: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["wasi-probe", "--target-dir"])
        .arg(target)
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
        .env(contextful_eval::record::MEASURE_DIR_VAR, records)
        .current_dir(repo_root())
        .output()
        .unwrap()
}

/// A failed `wasm32-wasip2` build records nothing and gates nothing: the probe exits 0 with
/// no record, so the scheduled tier runs its next entry.
#[test]
fn a_wasi_probe_whose_build_fails_records_nothing_and_exits_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, records) = (tmp.path().join("bin"), tmp.path().join("records"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&records).unwrap();
    wasi_cargo(&bin, 101, 0);
    let o = probe(&bin, &records, &tmp.path().join("target"));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("recorded nothing"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_dir(&records).unwrap().count(), 0, "a failed build left a record");
}

/// A built artifact is compressed and recorded under the ledger entry, and the probe's
/// build directory is reclaimed.
#[test]
fn a_wasi_probe_whose_build_succeeds_records_the_compressed_size() {
    let tmp = tempfile::tempdir().unwrap();
    let (bin, records, target) = (tmp.path().join("bin"), tmp.path().join("records"), tmp.path().join("target"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&records).unwrap();
    wasi_cargo(&bin, 0, 4096);
    let o = probe(&bin, &records, &target);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r = contextful_eval::record::read(&records, "edge-wasip2-footprint").unwrap();
    assert!(r.value > 0.0 && r.value < 1.0, "{}", r.value);
    assert!(!target.exists(), "the probe left {}", target.display());
}
