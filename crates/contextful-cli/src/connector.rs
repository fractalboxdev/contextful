//! `contextful connector pin` and the manifest a component source names: the guest builds on
//! the fixed builder platform (`connector.package.pin-verb`), `--verify` rebuilds and
//! compares, and `--local` prints a host-local digest it never writes
//! (`connector.package.host-built-pin`).

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use contextful_core::connector::component::ComponentSource;
use contextful_core::connector::manifest::Manifest;
use contextful_core::connector::package::{Digest, PLATFORM};
use contextful_core::connector::ConnectorError;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// The exit status of `--verify` when the rebuild's digest differs from the pin, distinct
/// from a failed build.
pub const DRIFT_STATUS: i32 = 3;

/// The container runtime, resolved on `PATH`.
const RUNTIME: &str = "docker";

#[derive(Subcommand)]
pub enum ConnectorCmd {
    /// Build a connector's guest on the fixed builder platform and write its artifact and digest.
    Pin {
        manifest: PathBuf,
        /// Build with the host toolchain and print a host-local digest, writing nothing.
        #[arg(long, conflicts_with = "verify")]
        local: bool,
        /// Rebuild and compare against the manifest's pin, writing nothing.
        #[arg(long)]
        verify: bool,
    },
}

pub fn run(command: ConnectorCmd) -> Result<()> {
    match command {
        ConnectorCmd::Pin { manifest, local, verify } => pin(&manifest, local, verify),
    }
}

fn pin(path: &Path, local: bool, verify: bool) -> Result<()> {
    let manifest = Manifest::load(path)?;
    let Some(source) = manifest.build_dir(path) else {
        bail!("connector manifest `{}` declares no `[build] source` to build", path.display());
    };
    let source = std::fs::canonicalize(&source).with_context(|| format!("reading build source `{}`", source.display()))?;
    let root = std::fs::canonicalize(path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new(".")))?;
    if !source.starts_with(&root) {
        return Err(ConnectorError::ConnectorManifestRejected(format!("`build.source` resolves to `{}`, outside the manifest directory", source.display())).into());
    }
    let out = tempfile::tempdir().context("creating a build directory")?;
    let out_dir = std::fs::canonicalize(out.path())?;
    if local {
        let digest = Digest::of(&build_local(&source, &out_dir)?);
        println!("{digest}");
        eprintln!("host-local digest; not a pin");
        return Ok(());
    }
    let wasm = build_container(&source, &out_dir)?;
    let digest = Digest::of(&wasm);
    if verify {
        let pinned = manifest.wasm_sha256.as_deref().and_then(Digest::parse);
        let Some(pinned) = pinned else {
            bail!("connector manifest `{}` carries no `wasm_sha256` to verify", path.display());
        };
        if pinned != digest {
            eprintln!("digest drift: `{}` pins {pinned} and the rebuild produced {digest}", path.display());
            // `exit` runs no destructor, so the build directory goes first.
            drop(out);
            std::process::exit(DRIFT_STATUS);
        }
        println!("{digest}");
        return Ok(());
    }
    let Some(artifact) = manifest.wasm.as_deref() else {
        bail!("connector manifest `{}` names no `wasm` artifact path to write", path.display());
    };
    if artifact.is_empty() || Path::new(artifact).components().any(|part| !matches!(part, Component::Normal(_))) {
        bail!("connector manifest `wasm` is a path below the manifest directory: `{artifact}`");
    }
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    replace(&dir.join(artifact), &wasm)?;
    let text = std::fs::read_to_string(path)?;
    replace(path, write_pin(&text, &digest)?.as_bytes())?;
    println!("{digest}");
    Ok(())
}

/// `text` with `wasm_sha256` set to `digest`, every comment and layout kept.
fn write_pin(text: &str, digest: &Digest) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("parsing connector manifest")?;
    match doc.get_mut("wasm_sha256").and_then(toml_edit::Item::as_value_mut) {
        Some(value) => {
            let decor = value.decor().clone();
            *value = digest.as_str().into();
            *value.decor_mut() = decor;
        }
        None => doc["wasm_sha256"] = toml_edit::value(digest.as_str()),
    }
    Ok(doc.to_string())
}

/// Write `bytes` to `path` through a sibling temporary file, so a reader sees the old or the new file.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    temporary.write_all(bytes)?;
    // The temporary file is created 0600; the result keeps the target's mode, or 0644 when new.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644);
        temporary.as_file().set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    temporary.persist(path).map_err(|e| e.error).with_context(|| format!("writing `{}`", path.display()))?;
    Ok(())
}

/// The one artifact a build left under `<out>/<target>/release`.
fn artifact(out: &Path) -> Result<Vec<u8>> {
    let release = out.join(PLATFORM.target).join("release");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&release).with_context(|| format!("reading build output `{}`", release.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "wasm") {
            found.push(path);
        }
    }
    match found.as_slice() {
        [one] => Ok(std::fs::read(one)?),
        _ => bail!("the build left {} artifacts under `{}`; a guest crate builds exactly one", found.len(), release.display()),
    }
}

/// Build in the platform's container as the invoking user, in two runs: `cargo fetch` with
/// network fills a per-build package cache, then the offline build reads it with no network.
/// The source mounts read-only and the package cache and toolchain remap. No answering
/// runtime refuses rather than building on the host.
fn build_container(source: &Path, out: &Path) -> Result<Vec<u8>> {
    let answered = Command::new(RUNTIME).arg("version").stdout(Stdio::null()).stderr(Stdio::null()).status();
    if !answered.is_ok_and(|status| status.success()) {
        return Err(ConnectorError::ConnectorHostPinRefused(format!(
            "no container runtime answers `{RUNTIME} version`; `--local` builds with the host toolchain and prints a host-local digest, never a pin"
        ))
        .into());
    }
    let target = out.join("target");
    let cache = out.join("cargo");
    std::fs::create_dir_all(&target)?;
    std::fs::create_dir_all(&cache)?;
    let remap = [(PLATFORM.image_cargo_home, PLATFORM.remap_cargo), (PLATFORM.image_rustup_home, PLATFORM.remap_rustup)]
        .iter()
        .map(|(from, to)| format!("--remap-path-prefix={from}={to}"))
        .collect::<Vec<_>>()
        .join(" ");
    let run = |offline: bool| -> Result<()> {
        let mut command = Command::new(RUNTIME);
        command.args(["run", "--rm", "--platform", PLATFORM.platform]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = std::fs::metadata(out)?;
            command.arg("--user").arg(format!("{}:{}", owner.uid(), owner.gid()));
        }
        command
            .arg("--volume")
            .arg(format!("{}:{}:ro", source.display(), PLATFORM.remap_source))
            .arg("--volume")
            .arg(format!("{}:{}", cache.display(), PLATFORM.remap_cargo))
            .args(["--workdir", PLATFORM.remap_source, "--env"])
            .arg(format!("CARGO_HOME={}", PLATFORM.remap_cargo));
        if offline {
            command
                .args(["--network", "none", "--volume"])
                .arg(format!("{}:/out", target.display()))
                .args(["--env", "CARGO_TARGET_DIR=/out", "--env"])
                .arg(format!("RUSTFLAGS={remap}"))
                .arg(PLATFORM.image)
                .args(PLATFORM.argv)
                .arg("--offline");
        } else {
            command.arg(PLATFORM.image).args(PLATFORM.fetch);
        }
        let status = command.status().with_context(|| format!("running `{RUNTIME}`"))?;
        if !status.success() {
            bail!("the container {} exited with {status}", if offline { "build" } else { "package fetch" });
        }
        Ok(())
    };
    run(false)?;
    run(true)?;
    artifact(&target)
}

/// Build with the host toolchain under the platform's argv and remap, the environment
/// cleared to `PATH` and `HOME` plus the toolchain's own locations.
fn build_local(source: &Path, out: &Path) -> Result<Vec<u8>> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("`--local` needs `HOME` to find the host toolchain")?;
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".cargo"));
    let rustup_home = std::env::var_os("RUSTUP_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".rustup"));
    let mut flags = OsString::new();
    for (from, to) in [(cargo_home.as_path(), PLATFORM.remap_cargo), (rustup_home.as_path(), PLATFORM.remap_rustup), (out, "/out"), (source, PLATFORM.remap_source)] {
        if !flags.is_empty() {
            flags.push("\u{1f}");
        }
        flags.push("--remap-path-prefix=");
        flags.push(from);
        flags.push(format!("={to}"));
    }
    let mut command = Command::new(PLATFORM.argv[0]);
    command.args(&PLATFORM.argv[1..]).current_dir(source).env_clear().env("HOME", &home).env("CARGO_TARGET_DIR", out).env("CARGO_ENCODED_RUSTFLAGS", flags);
    for name in ["PATH", "CARGO_HOME", "RUSTUP_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let status = command.status().context("running the host toolchain")?;
    if !status.success() {
        bail!("the host build exited with {status}");
    }
    artifact(out)
}

/// The manifest a component source names, read before the artifact loads; each operator
/// `allow` host must fit its declared host access (`connector.package.manifest-host-grant`).
pub fn admit(decl: &ComponentSource, base: &Path) -> Result<Manifest> {
    let Some(raw) = &decl.manifest else {
        return Err(ConnectorError::ConnectorManifestRejected("`manifest` is absent; a component source names its connector manifest".into()).into());
    };
    let relative = Path::new(raw);
    if relative.components().any(|part| !matches!(part, Component::Normal(_))) {
        bail!("component manifest is a project-relative file: `{raw}`");
    }
    let path = base.join(relative);
    let root = std::fs::canonicalize(base).context("reading project directory")?;
    let resolved = std::fs::canonicalize(&path).with_context(|| format!("reading connector manifest `{raw}`"))?;
    if !resolved.starts_with(&root) {
        bail!("component manifest is a project-relative file: `{raw}`");
    }
    let manifest = Manifest::load(&resolved).with_context(|| format!("connector manifest `{raw}`"))?;
    if !decl.allow.included_in(&manifest.declared.hosts) {
        let witness = decl.allow.widening_witness(&manifest.declared.hosts).unwrap_or_else(|| "an unresolved host".to_string());
        return Err(ConnectorError::ConnectorUndeclaredAccess(format!("host `{witness}` is absent from manifest `{raw}`")).into());
    }
    Ok(manifest)
}
