//! `assurance.build.release-profile`, `assurance.build.container-image`,
//! `assurance.build.release-image` and `assurance.gate.footprint-exceeded`: the release
//! profile and the container recipe of this repository, and `contextful-ci image` and
//! `contextful-ci footprint` over hand-built ELF artifacts.

use crate::{repo_root, stderr, Repo};
use std::process::{Command, Output};

/// Release builds compile with thin link-time optimization, one codegen unit per crate and symbols stripped, and unwind on panic.
// spec: assurance.build.release-profile@ab672851
#[test]
fn the_release_profile_optimizes_across_crates_strips_and_unwinds() {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&text).unwrap();
    let release = &manifest["profile"]["release"];
    assert_eq!(release["lto"].as_str(), Some("thin"), "{release:?}");
    assert_eq!(release["codegen-units"].as_integer(), Some(1), "{release:?}");
    assert_eq!(release["strip"].as_str(), Some("symbols"), "{release:?}");
    // The keeper survives a panicking job by unwinding to its guard (`run.cancel.keeper-panic`).
    assert!(release.get("panic").is_none_or(|p| p.as_str() == Some("unwind")), "{release:?}");
}

/// The instruction lines of the repository's `Dockerfile`, continuations joined.
fn instructions() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("Dockerfile")).unwrap();
    let mut out: Vec<String> = Vec::new();
    let mut open = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        open.push_str(line.trim_end_matches('\\').trim_end());
        open.push(' ');
        if !line.ends_with('\\') {
            out.push(open.trim().to_string());
            open.clear();
        }
    }
    out
}

#[test]
fn the_container_recipe_accepts_a_native_linux_release_target_without_changing_profiles() {
    let lines = instructions();
    assert!(lines.iter().any(|line| line == "ARG PLATFORM=linux/amd64"));
    assert_eq!(lines.iter().filter(|line| line.starts_with("FROM --platform=$PLATFORM ")).count(), 2);
    assert!(lines.iter().any(|line| line == "ARG RUST_TARGET=x86_64-unknown-linux-musl"));
    let build = lines.iter().find(|line| line.contains("cargo build")).unwrap();
    assert!(build.contains("--target \"$RUST_TARGET\""), "{build}");
    assert!(build.contains("target/$RUST_TARGET/release/contextful"), "{build}");
    assert!(lines.iter().any(|line| line.starts_with("RUN test \"$RUST_TARGET\"") && line.contains("rustc -vV")), "the target must match the pinned builder's native platform: {lines:?}");
}

/// The repository's `Dockerfile` builds one profile, `contextful-full` by default, as a static binary for the `PLATFORM` and `RUST_TARGET` its arguments name, and ships it in a shell-free runtime image as a non-root user over a declared store volume.
// spec: assurance.build.container-image@41f0245a
#[test]
fn the_container_recipe_builds_one_static_profile_into_a_shell_free_non_root_image() {
    let lines = instructions();
    let froms: Vec<&String> = lines.iter().filter(|l| l.starts_with("FROM ")).collect();
    assert_eq!(froms.len(), 2, "one builder and one runtime stage: {froms:?}");
    for from in &froms {
        assert!(from.contains("--platform=$PLATFORM"), "{from}");
        assert!(from.contains("@sha256:"), "a base image is pinned by digest: {from}");
    }
    assert!(froms[1].contains("distroless/static"), "the runtime stage carries no shell or C library: {}", froms[1]);

    let builder: Vec<&String> = lines.iter().skip_while(|l| !l.starts_with("FROM ")).skip(1).take_while(|l| !l.starts_with("FROM ")).collect();
    assert!(builder.iter().any(|l| *l == "ARG PROFILE=contextful-full"), "{builder:?}");
    let build = builder.iter().find(|l| l.contains("cargo build")).expect("the builder runs cargo build");
    for flag in ["--release", "--locked", "--no-default-features", "--features \"$PROFILE\"", "--target \"$RUST_TARGET\""] {
        assert!(build.contains(flag), "`{flag}` absent from: {build}");
    }
    assert!(
        builder.iter().any(|l| l.contains("contextful-ci footprint --profile \"$PROFILE\"")),
        "the builder holds the artifact to its profile's budget: {builder:?}"
    );

    let runtime: Vec<&String> = lines.iter().skip_while(|l| *l != froms[1]).collect();
    let user = runtime.iter().find_map(|l| l.strip_prefix("USER ")).expect("the runtime stage names a user");
    let uid: u32 = user.split(':').next().unwrap().parse().expect("a numeric uid");
    assert_ne!(uid, 0, "the image runs as root");
    assert!(runtime.iter().any(|l| l.starts_with("VOLUME ")), "{runtime:?}");
    assert!(runtime.iter().any(|l| l.starts_with("ENTRYPOINT [\"/usr/local/bin/contextful\"")), "{runtime:?}");
}

/// A minimal little-endian ELF64 shared object whose dynamic section names `needed`,
/// followed by `payload`.
fn elf(needed: &[&str], payload: &[u8]) -> Vec<u8> {
    let mut dynstr = vec![0u8];
    let mut dynamic = Vec::new();
    for lib in needed {
        dynamic.extend_from_slice(&1i64.to_le_bytes());
        dynamic.extend_from_slice(&(dynstr.len() as u64).to_le_bytes());
        dynstr.extend_from_slice(lib.as_bytes());
        dynstr.push(0);
    }
    dynamic.extend_from_slice(&[0u8; 16]);
    let dynstr_at = 64u64;
    let dynamic_at = dynstr_at + dynstr.len() as u64;
    let payload_at = dynamic_at + dynamic.len() as u64;
    let shoff = payload_at + payload.len() as u64;

    let mut out = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&0x3eu16.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&shoff.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for half in [64u16, 56, 0, 64, 3, 0] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    out.extend_from_slice(&dynstr);
    out.extend_from_slice(&dynamic);
    out.extend_from_slice(payload);
    let section = |kind: u32, offset: u64, size: u64, link: u32, entsize: u64| {
        let mut s = Vec::new();
        s.extend_from_slice(&0u32.to_le_bytes());
        s.extend_from_slice(&kind.to_le_bytes());
        for word in [0u64, 0, offset, size] {
            s.extend_from_slice(&word.to_le_bytes());
        }
        s.extend_from_slice(&link.to_le_bytes());
        s.extend_from_slice(&0u32.to_le_bytes());
        s.extend_from_slice(&1u64.to_le_bytes());
        s.extend_from_slice(&entsize.to_le_bytes());
        s
    };
    out.extend(section(0, 0, 0, 0, 0));
    out.extend(section(3, dynstr_at, dynstr.len() as u64, 0, 0));
    out.extend(section(6, dynamic_at, dynamic.len() as u64, 1, 16));
    out
}

/// `n` bytes no compressor shrinks, from a fixed-seed xorshift.
fn incompressible(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x5eed_0152;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A scratch repository whose assurance fragment budgets the edge profile at 1 MiB compressed.
fn budgeted() -> Repo {
    let r = Repo::init();
    r.write(
        "spec/terms/assurance.toml",
        "[limit]\nassurance-edge-compressed = { clause = \"assurance.gate.edge-budget\", value = 1, unit = \"MiB\", basis = \"chosen\", gloss = \"Compressed edge-profile artifact.\" }\n",
    );
    r
}

fn footprint(r: &Repo, profile: &str, artifact: &[u8]) -> Output {
    let path = r.root.join("contextful");
    std::fs::write(&path, artifact).unwrap();
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .args(["footprint", "--profile", profile])
        .arg(&path)
        .current_dir(&r.root)
        .output()
        .unwrap()
}

/// An artifact over its profile's budget, or carrying a dynamic dependency beyond the platform C library, raises `FootprintBudgetExceeded`, naming the profile.
// spec: assurance.gate.footprint-exceeded@b2d9b1e5
#[test]
fn an_artifact_over_budget_or_linking_beyond_the_c_library_is_refused() {
    let r = budgeted();
    // Compressible bulk and the platform C library alone hold.
    let o = footprint(&r, "contextful-edge", &elf(&["libc.so.6", "ld-linux-x86-64.so.2"], &vec![0u8; 4 << 20]));
    assert!(o.status.success(), "{}", stderr(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("footprint: `contextful-edge`") && out.contains("of 1 MiB") && out.contains("NEEDED libc.so.6, ld-linux-x86-64.so.2"), "{out}");

    let o = footprint(&r, "contextful-edge", &elf(&[], &incompressible(2 << 20)));
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.starts_with("FootprintBudgetExceeded: `contextful-edge`") && err.contains("over its 1 MiB budget"), "{err}");

    let o = footprint(&r, "contextful-edge", &elf(&["libc.so.6", "libstdc++.so.6"], b""));
    let err = stderr(&o);
    assert!(!o.status.success(), "{err}");
    assert!(err.starts_with("FootprintBudgetExceeded: `contextful-edge`") && err.contains("`libstdc++.so.6`"), "{err}");
    assert!(!err.contains("`libc.so.6`"), "{err}");

    // A profile without a budget, or an artifact that is no ELF file, measures nothing.
    let o = footprint(&r, "contextful-full", &elf(&[], b""));
    assert!(!o.status.success() && stderr(&o).contains("no `assurance-full-compressed` bound"), "{}", stderr(&o));
    let o = footprint(&r, "contextful-edge", b"#!/bin/sh\n");
    assert!(!o.status.success() && stderr(&o).contains("is no ELF64 file"), "{}", stderr(&o));
}

#[test]
fn footprint_records_exact_compressed_bytes_and_needed_entries_for_each_profile() {
    let r = budgeted();
    r.write(
        "spec/terms/assurance.toml",
        "[limit]\nassurance-control-compressed = { clause = \"assurance.gate.control-budget\", value = 1, unit = \"MiB\", basis = \"chosen\", gloss = \"Compressed control-profile artifact.\" }\nassurance-edge-compressed = { clause = \"assurance.gate.edge-budget\", value = 1, unit = \"MiB\", basis = \"chosen\", gloss = \"Compressed edge-profile artifact.\" }\nassurance-full-compressed = { clause = \"assurance.gate.full-budget\", value = 1, unit = \"MiB\", basis = \"chosen\", gloss = \"Compressed full-profile artifact.\" }\n",
    );
    let records = tempfile::tempdir().unwrap();
    let bytes = elf(&["libc.so.6", "ld-linux-x86-64.so.2"], b"sample payload");
    let artifact = r.root.join("contextful");
    std::fs::write(&artifact, &bytes).unwrap();
    for profile in ["control", "edge", "full"] {
        let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
            .args(["footprint", "--profile", &format!("contextful-{profile}")])
            .arg(&artifact)
            .current_dir(&r.root)
            .env(contextful_eval::record::MEASURE_DIR_VAR, records.path())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", stderr(&o));
        let compressed = contextful_eval::record::read(records.path(), &format!("profile-{profile}-compressed-bytes")).unwrap();
        assert_eq!(compressed.value, zstd::bulk::compress(&bytes, 19).unwrap().len() as f64);
        let needed = contextful_eval::record::read(records.path(), &format!("profile-{profile}-needed-entries")).unwrap();
        assert_eq!(needed.value, 2.0);
    }
}

/// Per nightly run and per profile, the footprint step builds the static-linked Linux target, compresses it, and holds its size to the profile's budget and its dynamic dependencies to the platform C library.
///
/// Under `contextful-ci measure --tier trend` the step builds all three profiles once,
/// recording each compressed byte count and NEEDED entry count alongside the number over
/// budget. Elsewhere, the workspace stage prints the planned builds and the budget stage
/// gates each artifact.
// spec: assurance.gate.footprint@76546e9a
#[test]
fn this_repository_profiles_hold_to_their_footprint_budgets() {
    let root = repo_root();
    let measuring = std::env::var_os(contextful_eval::record::MEASURE_DIR_VAR).is_some();
    let mut args = vec!["footprint", "--build"];
    if !measuring {
        args.push("--plan");
    }
    let o = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(&args).current_dir(root).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    for profile in ["contextful-control", "contextful-edge", "contextful-full"] {
        let line = out.lines().find(|l| l.starts_with(&format!("footprint: `{profile}` builds with "))).unwrap_or_else(|| panic!("no `{profile}` build: {out}"));
        for part in ["build --release --locked -p contextful-cli", &format!("--no-default-features --features {profile} --target"), "-unknown-linux-"] {
            assert!(line.contains(part), "`{part}` absent from: {line}");
        }
        // A glibc target links statically only under `crt-static`.
        assert!(!line.contains("-unknown-linux-gnu") || line.contains("RUSTFLAGS=-C target-feature=+crt-static"), "{line}");
    }
    if !measuring {
        assert!(o.status.success(), "{}", stderr(&o));
        return;
    }
    let over: u64 = out
        .lines()
        .find_map(|l| l.strip_prefix("footprint: ")?.strip_suffix(" of 3 profile(s) over budget")?.parse().ok())
        .unwrap_or_else(|| panic!("no count over budget: {out}\n{}", stderr(&o)));
    contextful_eval::record::emit("profile-footprint", over as f64, 3, 0);
    let _ = std::fs::remove_dir_all(root.join("target/footprint"));
    assert_eq!(over, 0, "{out}\n{}", stderr(&o));
    assert!(o.status.success(), "{}", stderr(&o));
}

/// The budget stage runs the footprint step over every profile, in one nightly-tier part per profile, and the evaluate stage builds no profile.
// spec: assurance.gate.budget-stage@8a2241c8
#[test]
fn the_budget_stage_builds_every_profile_one_part_each_and_the_evaluate_stage_none() {
    let stages = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("stages").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&stages.stdout).lines().last(), Some("budget"));
    let parts = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).args(["stages", "--parts", "--tier", "nightly"]).current_dir(repo_root()).output().unwrap();
    let listed = String::from_utf8_lossy(&parts.stdout).into_owned();
    let tail: Vec<&str> = listed.lines().rev().take(3).collect();
    assert_eq!(tail, ["budget.full", "budget.edge", "budget.control"], "{listed}");

    // A tree whose binary declares no profile builds nothing, and has no budget part.
    let r = Repo::init();
    r.lock();
    r.commit("lock");
    let o = r.gate(&["--stage", "budget"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("budget: no package declares a profile"), "{}", stderr(&o));
    let listed = String::from_utf8_lossy(&r.run_ci(&["stages", "--parts", "--tier", "nightly"]).stdout).into_owned();
    assert!(!listed.is_empty(), "the exclusion below ranges over no element");
    assert!(!listed.lines().any(|l| l.starts_with("budget")), "{listed}");

    // A binary declaring the three profiles under a fragment budgeting the edge alone: the
    // control part stops before it builds and reaches no other profile, and the whole stage
    // stops at the first profile.
    let r = budgeted();
    let features = "[features]\ncontextful-control = []\ncontextful-edge = []\ncontextful-full = []\n";
    r.write("crates/contextful-cli/Cargo.toml", &format!("{}{features}", crate::manifest("contextful-cli", "")));
    r.write("crates/contextful-cli/src/lib.rs", "");
    r.write("crates/contextful-cli/tests/integration/main.rs", "");
    r.lock();
    r.commit("a binary declaring the profiles");
    let o = r.gate(&["--stage", "budget.control"]);
    assert!(!o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("no `assurance-control-compressed` bound"), "{}", stderr(&o));
    assert!(!String::from_utf8_lossy(&o.stdout).contains("`contextful-edge`"), "the control part reached the edge profile");
    let o = r.gate(&["--stage", "budget"]);
    assert!(!o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("no `assurance-control-compressed` bound"), "{}", stderr(&o));

    let text = std::fs::read_to_string(repo_root().join("evals/ledger.toml")).unwrap();
    let ledger: toml::Value = toml::from_str(&text).unwrap();
    for (id, entry) in ledger["entry"].as_table().unwrap() {
        let test = entry.get("method").and_then(|m| m.get("test")).and_then(toml::Value::as_str).unwrap_or_default();
        let builds = test.ends_with("::this_repository_profiles_hold_to_their_footprint_budgets");
        assert!(!(builds && entry["tier"].as_str() == Some("gate")), "the gate-tier entry `{id}` builds every profile in the evaluate stage");
    }
}

const AMD64: &str = "x86_64-unknown-linux-musl";
const ARM64: &str = "aarch64-unknown-linux-musl";

/// A minimal static little-endian ELF64 executable for `machine` carrying `payload`, with one
/// `PT_INTERP` program header when `interpreter` holds.
fn executable(machine: u16, interpreter: bool, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&machine.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(if interpreter { 64u64 } else { 0 }).to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for half in [64u16, 56, u16::from(interpreter), 64, 0, 0] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    if interpreter {
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 52]);
    }
    out.extend_from_slice(payload);
    out
}

fn image(dir: &std::path::Path, binaries: &[(&str, &[u8])], out: &str) -> Output {
    let ca = dir.join("ca-certificates.crt");
    std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_contextful-ci"));
    c.args(["image", "--profile", "contextful-full", "--version", "0.0.1", "--ca-bundle"]).arg(&ca);
    for (i, (target, bytes)) in binaries.iter().enumerate() {
        let path = dir.join(format!("{out}-{i}.bin"));
        std::fs::write(&path, bytes).unwrap();
        c.arg("--binary").arg(format!("{target}={}", path.display()));
    }
    c.arg("--out").arg(dir.join(out)).current_dir(repo_root()).output().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

/// The runtime stage's argument to one instruction.
fn runtime(instruction: &str) -> String {
    let lines = instructions();
    let stage: Vec<&String> = lines.iter().rev().take_while(|l| !l.starts_with("FROM ")).collect();
    stage.iter().find_map(|l| l.strip_prefix(&format!("{instruction} "))).unwrap_or_else(|| panic!("no {instruction} in the runtime stage")).to_string()
}

/// Each profile's release image is an OCI index of a `linux/amd64` and a `linux/arm64` manifest; `contextful-ci image` assembles each from the profile's `x86_64` or `aarch64` musl release binary, with the `Dockerfile`'s runtime user, volume and entrypoint.
// spec: assurance.build.release-image@ac0e40ea
#[test]
fn the_release_image_indexes_an_amd64_and_an_arm64_manifest_assembled_from_release_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let amd = executable(0x3e, false, b"amd64 release binary");
    let arm = executable(0xb7, false, b"arm64 release binary");
    // Listed arm64 first: the index orders its platforms amd64 then arm64 whatever the order given.
    let o = image(dir.path(), &[(ARM64, &arm), (AMD64, &amd)], "both");
    assert!(o.status.success(), "{}", stderr(&o));
    let printed = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(printed.contains("image: `contextful-full` linux/amd64 manifest sha256:") && printed.contains("image: `contextful-full` linux/arm64 manifest sha256:"), "{printed}");

    let archive = dir.path().join("both/contextful-full-0.0.1.oci.tar");
    let layout = dir.path().join("layout");
    std::fs::create_dir_all(&layout).unwrap();
    let x = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&layout).output().unwrap();
    assert!(x.status.success(), "{}", stderr(&x));
    assert_eq!(json(&std::fs::read(layout.join("oci-layout")).unwrap())["imageLayoutVersion"], "1.0.0");
    let blob = |digest: &str| -> Vec<u8> {
        let hex = digest.strip_prefix("sha256:").unwrap();
        let bytes = std::fs::read(layout.join("blobs/sha256").join(hex)).unwrap_or_else(|e| panic!("{digest}: {e}"));
        assert_eq!(sha256_hex(&bytes), hex, "a blob named by another digest");
        bytes
    };

    // The layout names one image index, tagged with the version; it lists both platforms.
    let top = json(&std::fs::read(layout.join("index.json")).unwrap());
    let refs = top["manifests"].as_array().unwrap();
    assert_eq!(refs.len(), 1, "{top}");
    assert_eq!(refs[0]["mediaType"], "application/vnd.oci.image.index.v1+json");
    assert_eq!(refs[0]["annotations"]["org.opencontainers.image.ref.name"], "0.0.1");
    let index_bytes = blob(refs[0]["digest"].as_str().unwrap());
    assert_eq!(refs[0]["size"].as_u64(), Some(index_bytes.len() as u64));
    let index = json(&index_bytes);
    assert_eq!(index["schemaVersion"], 2);
    assert_eq!(index["mediaType"], "application/vnd.oci.image.index.v1+json");
    let entries = index["manifests"].as_array().unwrap();
    let platforms: Vec<(&str, &str)> = entries.iter().map(|e| (e["platform"]["os"].as_str().unwrap(), e["platform"]["architecture"].as_str().unwrap())).collect();
    assert_eq!(platforms, [("linux", "amd64"), ("linux", "arm64")], "{index}");
    assert_eq!(entries[1]["platform"]["variant"], "v8");

    let entrypoint: serde_json::Value = serde_json::from_str(&runtime("ENTRYPOINT")).unwrap();
    let cmd: serde_json::Value = serde_json::from_str(&runtime("CMD")).unwrap();
    let volumes: Vec<String> = serde_json::from_str(&runtime("VOLUME")).unwrap();
    for (entry, (arch, target, binary)) in entries.iter().zip([("amd64", AMD64, &amd), ("arm64", ARM64, &arm)]) {
        assert_eq!(entry["mediaType"], "application/vnd.oci.image.manifest.v1+json");
        let manifest_bytes = blob(entry["digest"].as_str().unwrap());
        assert_eq!(entry["size"].as_u64(), Some(manifest_bytes.len() as u64));
        let manifest = json(&manifest_bytes);
        assert_eq!(manifest["schemaVersion"], 2);
        assert_eq!(manifest["config"]["mediaType"], "application/vnd.oci.image.config.v1+json");
        let config = json(&blob(manifest["config"]["digest"].as_str().unwrap()));
        assert_eq!((config["os"].as_str(), config["architecture"].as_str()), (Some("linux"), Some(arch)), "{config}");
        assert_eq!(config["config"]["User"].as_str(), Some(runtime("USER").as_str()), "{config}");
        assert_eq!(config["config"]["WorkingDir"].as_str(), Some(runtime("WORKDIR").as_str()), "{config}");
        assert_eq!(config["config"]["Entrypoint"], entrypoint, "{config}");
        assert_eq!(config["config"]["Cmd"], cmd, "{config}");
        let declared: Vec<&String> = config["config"]["Volumes"].as_object().unwrap().keys().collect();
        assert_eq!(declared, volumes.iter().collect::<Vec<_>>(), "{config}");

        let layers = manifest["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1, "{manifest}");
        assert_eq!(layers[0]["mediaType"], "application/vnd.oci.image.layer.v1.tar+gzip");
        let layer = blob(layers[0]["digest"].as_str().unwrap());
        let gz = dir.path().join(format!("{arch}.layer.tar.gz"));
        std::fs::write(&gz, &layer).unwrap();
        let tar = Command::new("gzip").arg("-dc").arg(&gz).output().unwrap();
        assert!(tar.status.success(), "{}", stderr(&tar));
        assert_eq!(config["rootfs"]["diff_ids"][0].as_str(), Some(format!("sha256:{}", sha256_hex(&tar.stdout)).as_str()), "the diff id digests the uncompressed layer");
        let shipped = Command::new("tar").args(["-xzOf"]).arg(&gz).arg("usr/local/bin/contextful").output().unwrap();
        assert_eq!(&shipped.stdout, binary, "{target}: the layer ships another binary");
        let listed = Command::new("tar").args(["--numeric-owner", "-tvzf"]).arg(&gz).output().unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
        assert!(listed.lines().any(|l| l.ends_with("etc/ssl/certs/ca-certificates.crt")), "{listed}");
        let data = listed.lines().find(|l| l.trim_end_matches('/').ends_with(" data")).unwrap_or_else(|| panic!("no data directory: {listed}"));
        assert!(data.starts_with('d') && data.contains("65532"), "the store volume belongs to the runtime user: {data}");
        let bin = listed.lines().find(|l| l.ends_with("usr/local/bin/contextful")).unwrap();
        assert!(bin.starts_with("-rwxr-xr-x"), "{bin}");

        // Each platform's descriptor carries the blobs a registry push uploads and the index entry.
        let descriptor = json(&std::fs::read(dir.path().join(format!("both/contextful-full-0.0.1-linux-{arch}.image.json"))).unwrap());
        assert_eq!(descriptor["target"].as_str(), Some(target));
        assert_eq!(&descriptor["entry"], entry, "{descriptor}");
        assert_eq!(descriptor["manifest"]["content"].as_str().map(str::as_bytes), Some(&manifest_bytes[..]));
        assert_eq!(descriptor["config"]["digest"], manifest["config"]["digest"]);
        assert_eq!(descriptor["layer"]["digest"], layers[0]["digest"]);
        let file = descriptor["layer"]["path"].as_str().unwrap();
        assert_eq!(std::fs::read(dir.path().join("both").join(file)).unwrap(), layer, "{file}");
    }

    // One release cell per platform yields the same descriptor, so two cells compose the index.
    let o = image(dir.path(), &[(ARM64, &arm)], "cell");
    assert!(o.status.success(), "{}", stderr(&o));
    let cell = std::fs::read(dir.path().join("cell/contextful-full-0.0.1-linux-arm64.image.json")).unwrap();
    assert_eq!(cell, std::fs::read(dir.path().join("both/contextful-full-0.0.1-linux-arm64.image.json")).unwrap());
    let composed = serde_json::json!({ "schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json", "manifests": entries });
    assert_eq!(serde_json::to_vec(&composed).unwrap(), index_bytes, "the index is its entries, amd64 then arm64");

    // The same binaries assemble the same archive, byte for byte.
    let o = image(dir.path(), &[(AMD64, &amd), (ARM64, &arm)], "again");
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(std::fs::read(dir.path().join("again/contextful-full-0.0.1.oci.tar")).unwrap(), std::fs::read(&archive).unwrap());
}

/// A binary whose ELF machine differs from its target's, or whose program headers name a dynamic interpreter, raises `ImageBinaryMismatch`, naming the target.
// spec: assurance.build.image-binary-mismatch@f8fa386a
#[test]
fn an_image_binary_of_another_machine_or_naming_an_interpreter_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let refused = |binaries: &[(&str, &[u8])], out: &str, needle: &str| {
        let o = image(dir.path(), binaries, out);
        let err = stderr(&o);
        assert!(!o.status.success(), "{err}");
        assert!(err.starts_with(&format!("ImageBinaryMismatch: `{}`", binaries.last().unwrap().0)) && err.contains(needle), "{err}");
        assert!(!dir.path().join(out).join("contextful-full-0.0.1.oci.tar").exists(), "a refused image left an archive");
    };
    let amd = executable(0x3e, false, b"x");
    refused(&[(AMD64, &amd), (ARM64, &amd)], "machine", "machine");
    refused(&[(AMD64, &executable(0x3e, true, b"x"))], "interp", "interpreter");
    refused(&[(ARM64, b"#!/bin/sh\n")], "script", "no ELF64");
}
