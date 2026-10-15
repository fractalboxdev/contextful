//! `contextful connector pin`: the build on the fixed builder platform, `--verify`, and the
//! host-local `--local` build. A stand-in `docker` on `PATH` plays the container runtime.
#![cfg(unix)]

use contextful_core::connector::package::PLATFORM;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The probe guest exporting `config` and `attribution` beside the base world.
const PROBE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe.wasm");
/// The probe guest exporting the base world alone.
const PROBE_BASE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contextful-wasm/tests/fixtures/probe-base.wasm");

/// A stand-in container runtime: answers `version`, records its argument vector, and
/// copies `$FAKE_DOCKER_WASM` into the output directory mounted at `/out`.
const FAKE_DOCKER: &str = r#"#!/bin/sh
if [ "$1" = "version" ]; then
  exit 0
fi
printf '%s\n' "$@" > "$FAKE_DOCKER_RECORD"
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--volume" ]; then
    case "$arg" in
      *:/out) out="${arg%:/out}" ;;
    esac
  fi
  prev="$arg"
done
mkdir -p "$out/wasm32-wasip2/release"
cp "$FAKE_DOCKER_WASM" "$out/wasm32-wasip2/release/guest.wasm"
"#;

const MANIFEST: &str = "# The vendor connector.\nname = \"probe\"\nwasm = \"probe.wasm\" # written by `connector pin`\n\n[build]\nsource = \"guest\"\n\n[capabilities]\nclock = true\n";

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct Fixture {
    dir: tempfile::TempDir,
    bin: PathBuf,
    record: PathBuf,
}

/// A connector directory holding `manifest` and an empty guest crate, and a `bin` directory
/// holding the stand-in runtime.
fn fixture(manifest: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("connector/guest/src")).unwrap();
    std::fs::write(dir.path().join("connector/connector.toml"), manifest).unwrap();
    std::fs::write(dir.path().join("connector/guest/Cargo.toml"), "[package]\nname = \"guest\"\n").unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let docker = bin.join("docker");
    std::fs::write(&docker, FAKE_DOCKER).unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let record = dir.path().join("argv");
    Fixture { dir, bin, record }
}

impl Fixture {
    fn connector(&self) -> PathBuf {
        self.dir.path().join("connector")
    }

    /// `contextful connector pin connector.toml <extra>` with `path` as `PATH` and `wasm` as
    /// what the runtime builds.
    fn pin(&self, path: &str, wasm: &str, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(["connector", "pin", "connector.toml"])
            .args(extra)
            .current_dir(self.connector())
            .env("PATH", path)
            .env("FAKE_DOCKER_RECORD", &self.record)
            .env("FAKE_DOCKER_WASM", wasm)
            .output()
            .unwrap()
    }

    fn with_runtime(&self) -> String {
        format!("{}:/bin:/usr/bin", self.bin.display())
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.record).unwrap().lines().map(str::to_string).collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn the_stand_in_runtime_passes_shellcheck() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("docker");
    std::fs::write(&script, FAKE_DOCKER).unwrap();
    let out = Command::new("shellcheck").arg(&script).output().expect("shellcheck runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
}

/// `contextful connector pin <manifest>` builds the guest in a digest-pinned container on one fixed platform and writes
/// the artifact and its digest. `--verify` rebuilds and compares without writing.
#[test]
fn pin_builds_on_the_fixed_platform_and_writes_the_artifact_and_digest() {
    assert!(PLATFORM.image.contains("@sha256:") && PLATFORM.image.rsplit('@').next().unwrap().len() == 71, "{}", PLATFORM.image);
    let f = fixture(MANIFEST);
    let out = f.pin(&f.with_runtime(), PROBE, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let pin = digest(&std::fs::read(PROBE).unwrap());
    assert!(String::from_utf8_lossy(&out.stdout).contains(&pin));

    let argv = f.argv();
    let source = std::fs::canonicalize(f.connector().join("guest")).unwrap();
    assert_eq!(argv[0], "run");
    for pair in [["--platform", "linux/amd64"], ["--network", "none"]] {
        assert!(argv.windows(2).any(|w| w == pair), "{pair:?} in {argv:?}");
    }
    assert!(argv.windows(2).any(|w| w[0] == "--volume" && w[1] == format!("{}:/src:ro", source.display())), "{argv:?}");
    let image = argv.iter().position(|a| a == PLATFORM.image).expect("the platform image");
    assert_eq!(argv[image + 1..], PLATFORM.argv.iter().map(|s| s.to_string()).collect::<Vec<_>>()[..]);

    let written = std::fs::read_to_string(f.connector().join("connector.toml")).unwrap();
    assert!(written.contains(&format!("wasm_sha256 = \"{pin}\"")), "{written}");
    assert!(written.contains("# The vendor connector.") && written.contains("# written by `connector pin`"), "comments survive: {written}");
    assert_eq!(std::fs::read(f.connector().join("probe.wasm")).unwrap(), std::fs::read(PROBE).unwrap());

    // `--verify` against the same build writes nothing and succeeds.
    let before = std::fs::read(f.connector().join("connector.toml")).unwrap();
    let out = f.pin(&f.with_runtime(), PROBE, &["--verify"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(std::fs::read(f.connector().join("connector.toml")).unwrap(), before);
}

#[test]
fn verify_on_drift_names_both_digests_and_writes_nothing() {
    let f = fixture(MANIFEST);
    assert!(f.pin(&f.with_runtime(), PROBE, &[]).status.success());
    let manifest = std::fs::read(f.connector().join("connector.toml")).unwrap();
    let out = f.pin(&f.with_runtime(), PROBE_BASE, &["--verify"]);
    assert_eq!(out.status.code(), Some(3), "drift exits with its own status: {}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains(&digest(&std::fs::read(PROBE).unwrap())) && text.contains(&digest(&std::fs::read(PROBE_BASE).unwrap())), "{text}");
    assert_eq!(std::fs::read(f.connector().join("connector.toml")).unwrap(), manifest);
    assert_eq!(std::fs::read(f.connector().join("probe.wasm")).unwrap(), std::fs::read(PROBE).unwrap());
}

/// `--local` builds with the host toolchain and prints a host-local digest; writing a pin from a host build raises
/// `ConnectorHostPinRefused`.
#[test]
fn pin_without_a_container_runtime_refuses_and_never_builds_on_the_host() {
    let f = fixture(MANIFEST);
    let empty = f.dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let out = f.pin(&empty.display().to_string(), PROBE, &[]);
    let text = stderr(&out);
    assert!(!out.status.success());
    assert!(text.contains("ConnectorHostPinRefused") && text.contains("--local"), "{text}");
    assert_eq!(std::fs::read_to_string(f.connector().join("connector.toml")).unwrap(), MANIFEST);
    assert!(!f.connector().join("probe.wasm").exists());
}

/// A manifest's `build.source` is its only build input; the fixed builder platform holds image, target, compiler,
/// package manager and command, and a `build.source` leaving the manifest directory refuses under
/// {{connector.package.manifest-keys}}.
#[test]
fn a_build_source_outside_the_manifest_directory_builds_nothing() {
    let f = fixture(&MANIFEST.replace("source = \"guest\"", "source = \"../guest\""));
    let out = f.pin(&f.with_runtime(), PROBE, &[]);
    let text = stderr(&out);
    assert!(text.contains("ConnectorManifestRejected") && text.contains("`build.source`"), "{text}");
    assert!(!f.record.exists(), "the runtime was invoked");
}

const GUEST_TOML: &str = "[package]\nname = \"guest\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[profile.release]\ndebug = true\n\n[workspace]\n";
const GUEST_LOCK: &str = "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"guest\"\nversion = \"0.1.0\"\n";
const GUEST_LIB: &str = "#[no_mangle]\npub extern \"C\" fn answer(x: i32) -> i32 {\n    if x < 0 {\n        panic!(\"negative\");\n    }\n    x * 2\n}\n";

/// A checkout of the dependency-free guest under `root`, its manifest naming it.
fn checkout(root: &Path) -> PathBuf {
    let connector = root.join("a-checkout-path-that-must-not-leak");
    std::fs::create_dir_all(connector.join("guest/src")).unwrap();
    std::fs::write(connector.join("guest/Cargo.toml"), GUEST_TOML).unwrap();
    std::fs::write(connector.join("guest/Cargo.lock"), GUEST_LOCK).unwrap();
    std::fs::write(connector.join("guest/src/lib.rs"), GUEST_LIB).unwrap();
    std::fs::write(connector.join("connector.toml"), MANIFEST).unwrap();
    std::fs::canonicalize(connector).unwrap()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Every build remaps the source root and the package cache, so checkout location is not a digest input.
#[test]
fn a_local_build_from_two_checkouts_prints_one_digest_and_writes_nothing() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let checkouts: Vec<PathBuf> = roots.iter().map(|r| checkout(r.path())).collect();

    // Without the remap, a build embeds its checkout path: the guard below proves something.
    let target = tempfile::tempdir().unwrap();
    let plain = Command::new("cargo")
        .args(["build", "--release", "--locked", "--target", "wasm32-wasip2", "--manifest-path"])
        .arg(checkouts[0].join("guest/Cargo.toml"))
        .env("CARGO_TARGET_DIR", target.path())
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .output()
        .unwrap();
    assert!(plain.status.success(), "the wasm32-wasip2 target builds: {}", stderr(&plain));
    let bytes = std::fs::read(target.path().join("wasm32-wasip2/release/guest.wasm")).unwrap();
    assert!(contains(&bytes, checkouts[0].to_string_lossy().as_bytes()), "a plain build embeds the checkout path");

    let mut digests = Vec::new();
    for connector in &checkouts {
        let out = Command::new(env!("CARGO_BIN_EXE_contextful"))
            .args(["connector", "pin", "connector.toml", "--local"])
            .current_dir(connector)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(stderr(&out).contains("host-local digest; not a pin"), "{}", stderr(&out));
        let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(printed.len() == 64 && printed.bytes().all(|b| b.is_ascii_hexdigit()), "{printed}");
        assert_eq!(std::fs::read_to_string(connector.join("connector.toml")).unwrap(), MANIFEST, "--local writes nothing");
        assert!(!connector.join("probe.wasm").exists());
        digests.push(printed);
    }
    assert_eq!(digests[0], digests[1], "checkout location moved the digest");
}
