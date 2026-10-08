//! `assurance.build.targets` and `assurance.build.release-artifact`: `contextful-ci release`
//! over this workspace, with `cargo build` answered by a script that writes a stand-in
//! binary and every `cargo tree` resolved by the real cargo.

use crate::repo_root;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Command, Output};

const LINUX_X86: &str = "x86_64-unknown-linux-musl";
const WINDOWS: [&str; 2] = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];

#[test]
fn windows_release_plan_contains_both_msvc_targets_for_edge_and_full_only() {
    let out = ci(&["release", "--plan"], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let cells: Vec<_> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect();
    assert_eq!(cells.len(), 14, "release plan omits Windows cells: {cells:?}");
    for target in WINDOWS {
        for profile in ["contextful-edge", "contextful-full"] {
            assert_eq!(cells.iter().filter(|cell| **cell == format!("{profile} {target}")).count(), 1);
        }
        assert!(!cells.contains(&format!("contextful-control {target}")));
    }
}

#[test]
fn windows_release_preserves_executable_bytes_license_digest_and_sbom() {
    let bin = tempfile::tempdir().unwrap();
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let script = format!("#!/bin/sh\nif [ \"$1\" = build ]; then\n t=''; prev=''; for a in \"$@\"; do [ \"$prev\" = --target ] && t=\"$a\"; prev=\"$a\"; done\n mkdir -p \"$CARGO_TARGET_DIR/$t/release\"\n printf 'MZ\\000fixture\\377' > \"$CARGO_TARGET_DIR/$t/release/contextful.exe\"\n exit 0\nfi\nexec '{real}' \"$@\"\n");
    let cargo = bin.path().join("cargo");
    std::fs::write(&cargo, script).unwrap();
    std::fs::set_permissions(cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let dist = tempfile::tempdir().unwrap();
    for target in WINDOWS {
        for profile in ["contextful-edge", "contextful-full"] {
            let out = ci(&["release", "--profile", profile, "--target", target, "--target-dir", target_dir.path().to_str().unwrap(), "--out", dist.path().to_str().unwrap()], Some(bin.path()));
            assert!(out.status.success(), "{profile} {target}: {}", String::from_utf8_lossy(&out.stderr));
            let stem = format!("{}-{}-{target}", profile, version());
            let archive = dist.path().join(format!("{stem}.tar.gz"));
            let binary = Command::new("tar").args(["-xzOf"]).arg(&archive).arg(format!("{stem}/contextful.exe")).output().unwrap();
            assert!(binary.status.success());
            assert_eq!(binary.stdout, b"MZ\0fixture\xff");
            let license = Command::new("tar").args(["-xzOf"]).arg(&archive).arg(format!("{stem}/LICENSE")).output().unwrap();
            assert!(license.status.success());
            assert_eq!(license.stdout, std::fs::read(repo_root().join("LICENSE")).unwrap());
            let digest = format!("{:x}", Sha256::digest(std::fs::read(&archive).unwrap()));
            assert_eq!(std::fs::read_to_string(dist.path().join(format!("{stem}.tar.gz.sha256"))).unwrap(), format!("{digest}  {stem}.tar.gz\n"));
            let metadata: serde_json::Value = serde_json::from_slice(&std::fs::read(dist.path().join(format!("{stem}.release.json"))).unwrap()).unwrap();
            assert_eq!(metadata["target"], target);
            assert_eq!(metadata["sha256"], digest);
            let sbom: serde_json::Value = serde_json::from_slice(&std::fs::read(dist.path().join(format!("{stem}.cdx.json"))).unwrap()).unwrap();
            assert!(sbom["metadata"]["properties"].as_array().unwrap().iter().any(|property| property["name"] == "contextful:target" && property["value"] == target));
        }
    }
}

fn complete_release_metadata() -> Vec<serde_json::Value> {
    let mut cells = Vec::new();
    for profile in ["contextful-control", "contextful-edge", "contextful-full"] {
        let mut targets = vec![LINUX_X86, "aarch64-unknown-linux-musl"];
        if profile != "contextful-control" {
            targets.extend(["aarch64-apple-darwin", "x86_64-apple-darwin"]);
            targets.extend(WINDOWS);
        }
        for target in targets {
            let stem = format!("{profile}-{}-{target}", version());
            cells.push(serde_json::json!({"profile":profile,"target":target,"archive":format!("{stem}.tar.gz"),"sbom":format!("{stem}.cdx.json"),"sha256":"a".repeat(64)}));
        }
    }
    cells
}

#[test]
fn windows_release_metadata_is_required_once_and_never_becomes_a_homebrew_platform() {
    let dist = tempfile::tempdir().unwrap();
    let manifest = dist.path().join("manifest.json");
    let cells = complete_release_metadata();
    let formula = |cells: &[serde_json::Value]| {
        std::fs::write(&manifest, serde_json::to_vec(cells).unwrap()).unwrap();
        ci(&["formula", "--manifest", manifest.to_str().unwrap(), "--dist", dist.path().to_str().unwrap(), "--base-url", "https://example.com/v"], None)
    };
    let out = formula(&cells);
    assert!(out.status.success(), "complete Windows metadata refuses: {}", String::from_utf8_lossy(&out.stderr));
    let sums = std::fs::read_to_string(dist.path().join("SHA256SUMS")).unwrap();
    assert_eq!(sums.lines().count(), 14);
    assert_eq!(sums.lines().filter(|line| line.contains("windows-msvc")).count(), 4);
    for name in ["contextful", "contextful-control", "contextful-edge", "contextful-full"] {
        let text = std::fs::read_to_string(dist.path().join(format!("Formula/{name}.rb"))).unwrap();
        assert!(!text.contains("windows"), "Homebrew contains a Windows cell: {text}");
    }
    let mut invalid = cells.clone();
    invalid.pop();
    assert!(!formula(&invalid).status.success(), "missing Windows metadata publishes");
    let mut invalid = cells.clone();
    invalid.push(cells.last().unwrap().clone());
    assert!(!formula(&invalid).status.success(), "duplicate Windows metadata publishes");
    let mut invalid = cells.clone();
    invalid.last_mut().unwrap()["archive"] = serde_json::json!("wrong.tar.gz");
    assert!(!formula(&invalid).status.success(), "wrong Windows archive publishes");
    let mut invalid = cells.clone();
    invalid.last_mut().unwrap()["sbom"] = serde_json::json!("wrong.cdx.json");
    assert!(!formula(&invalid).status.success(), "wrong Windows SBOM publishes");
    let mut invalid = cells.clone();
    invalid.last_mut().unwrap()["sha256"] = serde_json::json!("invalid");
    assert!(!formula(&invalid).status.success(), "invalid Windows digest publishes");
}

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
        "#!/bin/sh\nif [ \"$1\" = build ] || [ \"$1\" = zigbuild ]; then\n  t=''; prev=''\n  for a in \"$@\"; do [ \"$prev\" = --target ] && t=\"$a\"; prev=\"$a\"; done\n  \
         mkdir -p \"$CARGO_TARGET_DIR/$t/release\"\n  name=contextful; case \"$t\" in *-windows-msvc) name=contextful.exe;; esac\n  echo \"$*\" > \"$CARGO_TARGET_DIR/$t/release/$name\"\n  exit 0\nfi\nexec '{real}' \"$@\"\n"
    );
    let path = dir.join("cargo");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn default_release_and_footprint_targets_follow_the_inherited_pool() {
    let bin = tempfile::tempdir().unwrap();
    fake_cargo(bin.path());
    let pool = tempfile::tempdir().unwrap();
    let dist = tempfile::tempdir().unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["release", "--profile", "contextful-edge", "--target", "aarch64-apple-darwin", "--out"])
        .arg(dist.path())
        .current_dir(repo_root())
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", pool.path())
        .output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(pool.path().join("contextful-ci/release-artifacts/aarch64-apple-darwin/release/contextful").exists());

    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["footprint", "--build", "--plan", "--profile", "contextful-edge"])
        .current_dir(repo_root())
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", pool.path())
        .output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let plan = String::from_utf8_lossy(&o.stdout);
    assert!(plan.contains(pool.path().join("contextful-ci/footprint").to_str().unwrap()), "{plan}");
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
        for t in ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
            expected.push(format!("{p} {t}"));
        }
    }
    expected.sort();
    assert_eq!(cells, expected);

    let wasi = ci(&["release", "--target", "wasm32-wasip2", "--plan"], None);
    assert!(!wasi.status.success(), "a wasm32-wasip2 release target is accepted");
}

/// The release command uses `cargo build` by default and `cargo zigbuild` under `--builder zigbuild`, forwarding the selected target and profile features.
// spec: assurance.build.release-builder@8af6b703
/// Each release cell writes a JSON record naming its profile, target, archive, SHA-256 digest and SBOM.
// spec: assurance.build.release-metadata@6d2d917d
#[test]
fn zigbuild_packages_a_darwin_cell_and_emits_its_formula_metadata() {
    let bin = tempfile::tempdir().unwrap();
    fake_cargo(bin.path());
    let target = tempfile::tempdir().unwrap();
    let dist = tempfile::tempdir().unwrap();
    let triple = "aarch64-apple-darwin";
    let run = ci(&["release", "--builder", "zigbuild", "--profile", "contextful-edge", "--target", triple, "--target-dir", target.path().to_str().unwrap(), "--out", dist.path().to_str().unwrap()], Some(bin.path()));
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let stem = format!("contextful-edge-{}-{triple}", version());
    let binary = Command::new("tar").args(["-xzOf"]).arg(dist.path().join(format!("{stem}.tar.gz"))).arg(format!("{stem}/contextful")).output().unwrap();
    assert!(binary.status.success());
    assert!(String::from_utf8_lossy(&binary.stdout).starts_with("zigbuild --release --locked"));
    let metadata: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dist.path().join(format!("{stem}.release.json"))).unwrap()).unwrap();
    assert_eq!(metadata["profile"], "contextful-edge");
    assert_eq!(metadata["target"], triple);
    assert_eq!(metadata["archive"], format!("{stem}.tar.gz"));
    assert_eq!(metadata["sbom"], format!("{stem}.cdx.json"));
    assert_eq!(metadata["sha256"].as_str().unwrap().len(), 64);
}

/// The formula command accepts a manifest covering every release matrix cell once, with matching asset names and SHA-256 digests, and writes formulas and SHA256SUMS without local archives.
// spec: assurance.build.formula-manifest@8b3c9e8b
#[test]
fn formula_uses_metadata_without_local_release_archives() {
    let dist = tempfile::tempdir().unwrap();
    let manifest = dist.path().join("release-manifest.json");
    let out = ci(&["release", "--plan"], None);
    assert!(out.status.success());
    let cells: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout).lines().map(|line| {
        let (profile, target) = line.split_once(' ').unwrap();
        let short = profile.strip_prefix("contextful-").unwrap();
        let stem = format!("contextful-{short}-{}-{target}", version());
        serde_json::json!({"profile": profile, "target": target, "archive": format!("{stem}.tar.gz"), "sha256": "a".repeat(64), "sbom": format!("{stem}.cdx.json")})
    }).collect();
    std::fs::write(&manifest, serde_json::to_vec(&cells).unwrap()).unwrap();
    let run = ci(&["formula", "--manifest", manifest.to_str().unwrap(), "--dist", dist.path().to_str().unwrap(), "--base-url", "https://example.com/v"], None);
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let sums = std::fs::read_to_string(dist.path().join("SHA256SUMS")).unwrap();
    assert_eq!(sums.lines().count(), 14);
    assert!(sums.lines().all(|line| line.starts_with(&"a".repeat(64))));
    let formula = std::fs::read_to_string(dist.path().join("Formula/contextful-full.rb")).unwrap();
    assert!(formula.contains("https://example.com/v/contextful-full-"));
    assert!(formula.contains(&"a".repeat(64)));

    std::fs::write(&manifest, serde_json::to_vec(&cells[..13]).unwrap()).unwrap();
    let incomplete = ci(&["formula", "--manifest", manifest.to_str().unwrap(), "--dist", dist.path().to_str().unwrap(), "--base-url", "https://example.com/v"], None);
    assert!(!incomplete.status.success(), "a missing release cell produced formulae");
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
    for t in ["aarch64-unknown-linux-musl", "aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
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
    assert_eq!(sums.lines().count(), 14, "{sums}");

    let install = std::fs::read_to_string(repo_root().join("install.sh")).unwrap();
    assert!(install.contains("profile=\"${CONTEXTFUL_PROFILE:-contextful-full}\""), "the install script defaults to another profile");
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
