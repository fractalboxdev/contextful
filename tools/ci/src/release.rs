//! `contextful-ci release` (`assurance.build.targets`, `assurance.build.release-artifact`):
//! each profile built for a release target and packaged as an archive beside its SHA-256
//! checksum and a CycloneDX SBOM; `contextful-ci formula` writes the package-manager
//! formulae over a directory of them.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The binary every profile builds.
const BINARY: &str = "contextful";
const PACKAGE: &str = "contextful-cli";

const LINUX: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];
const DARWIN: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const WINDOWS: [&str; 2] = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];

/// The profile the bare formula name and the install script resolve to.
pub const DEFAULT_PROFILE: &str = "contextful-full";

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Builder {
    Cargo,
    Zigbuild,
}

impl Builder {
    fn subcommand(self) -> &'static str {
        match self {
            Self::Cargo => "build",
            Self::Zigbuild => "zigbuild",
        }
    }
}

/// Every profile with the targets it ships for: all three cross-compile to Linux on musl,
/// and edge and full also build for macOS and Windows on MSVC.
pub fn matrix() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("contextful-control", LINUX.to_vec()),
        ("contextful-edge", LINUX.iter().chain(DARWIN.iter()).chain(WINDOWS.iter()).copied().collect()),
        ("contextful-full", LINUX.iter().chain(DARWIN.iter()).chain(WINDOWS.iter()).copied().collect()),
    ]
}

/// The release target this host builds natively: musl, Apple or MSVC.
pub fn host_target() -> Result<String> {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => Ok(format!("{arch}-unknown-linux-musl")),
        "macos" => Ok(format!("{arch}-apple-darwin")),
        "windows" => Ok(format!("{arch}-pc-windows-msvc")),
        other => bail!("no release target builds on `{other}`; name one with `--target`"),
    }
}

/// `profile` without its `contextful-` prefix, as archive names carry it.
fn short(profile: &str) -> &str {
    profile.strip_prefix("contextful-").unwrap_or(profile)
}

/// The archive stem of `profile` at `version` for `target`.
pub fn stem(profile: &str, version: &str, target: &str) -> String {
    format!("{BINARY}-{}-{version}-{target}", short(profile))
}

/// The cargo invocation building `profile` alone for `target`.
fn cargo_build(profile: &str, target: &str, builder: Builder) -> Vec<String> {
    [builder.subcommand(), "--release", "--locked", "-p", PACKAGE, "--bin", BINARY, "--no-default-features", "--features", profile, "--target", target]
        .iter()
        .map(|a| a.to_string())
        .collect()
}

/// The workspace version, from `[workspace.package]`.
pub fn version(root: &Path) -> Result<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).context("reading Cargo.toml")?;
    let doc: toml::Value = toml::from_str(&text).context("parsing Cargo.toml")?;
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .context("Cargo.toml declares no [workspace.package] version")
}

/// The (profile, target) cells `profiles` and `targets` select from the matrix. A named
/// profile or target the matrix does not carry refuses; a profile not shipping for a named
/// target is left out of it.
fn cells(profiles: &[String], targets: &[String]) -> Result<Vec<(&'static str, &'static str)>> {
    let m = matrix();
    for p in profiles {
        if !m.iter().any(|(q, _)| q == p) {
            bail!("`{p}` is no release profile; the profiles are {}", m.iter().map(|(q, _)| *q).collect::<Vec<_>>().join(", "));
        }
    }
    let known: BTreeSet<&str> = m.iter().flat_map(|(_, t)| t.iter().copied()).collect();
    for t in targets {
        if !known.contains(t.as_str()) {
            bail!("`{t}` is no release target; the targets are {}", known.iter().copied().collect::<Vec<_>>().join(", "));
        }
    }
    let mut out = Vec::new();
    for (p, ts) in &m {
        if !profiles.is_empty() && !profiles.iter().any(|q| q == p) {
            continue;
        }
        for t in ts {
            if targets.iter().any(|u| u == t) {
                out.push((*p, *t));
            }
        }
    }
    Ok(out)
}

/// Print each (profile, target) cell `profiles` and `targets` select, every cell when both
/// are empty, one per line.
pub fn plan(profiles: &[String], targets: &[String]) -> Result<()> {
    let targets: Vec<String> =
        if targets.is_empty() { matrix().iter().flat_map(|(_, t)| t.iter().map(|t| t.to_string())).collect() } else { targets.to_vec() };
    let mut seen = BTreeSet::new();
    for (p, t) in cells(profiles, &targets)? {
        if seen.insert((p, t)) {
            println!("{p} {t}");
        }
    }
    Ok(())
}

/// Build each selected profile for each selected target, `targets` defaulting to this host's,
/// into `target_dir`, and package each artifact into `out`.
pub fn release(root: &Path, builder: Builder, profiles: &[String], targets: &[String], target_dir: &Path, out: &Path) -> Result<()> {
    let targets = if targets.is_empty() { vec![host_target()?] } else { targets.to_vec() };
    let selected = cells(profiles, &targets)?;
    if selected.is_empty() {
        bail!("no profile ships for {}", targets.join(", "));
    }
    let version = version(root)?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    for (profile, target) in selected {
        let args = cargo_build(profile, target, builder);
        eprintln!("release: cargo {}", args.join(" "));
        let status = Command::new("cargo")
            .args(&args)
            .env("CARGO_TARGET_DIR", target_dir)
            .current_dir(root)
            .status()
            .context("running cargo")?;
        if !status.success() {
            bail!("`cargo {}` exited {}", args.join(" "), status.code().unwrap_or(-1));
        }
        let binary = target_dir.join(target).join("release").join(binary_name(target));
        package(root, profile, &version, target, &binary, out)?;
    }
    Ok(())
}

fn binary_name(target: &str) -> &'static str {
    if WINDOWS.contains(&target) { "contextful.exe" } else { BINARY }
}

/// Write `<stem>.tar.gz` holding the target's binary name and licence, `<stem>.tar.gz.sha256`
/// in `sha256sum` form, and `<stem>.cdx.json`, the SBOM of the profile's resolved graph.
fn package(root: &Path, profile: &str, version: &str, target: &str, binary: &Path, out: &Path) -> Result<()> {
    let stem = stem(profile, version, target);
    let staging = out.join(&stem);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    std::fs::copy(binary, staging.join(binary_name(target))).with_context(|| format!("copying {}", binary.display()))?;
    std::fs::copy(root.join("LICENSE"), staging.join("LICENSE")).context("copying LICENSE")?;
    let archive = format!("{stem}.tar.gz");
    let status = Command::new("tar")
        .args(["-czf", &archive, &stem])
        .current_dir(out)
        .status()
        .context("running tar")?;
    std::fs::remove_dir_all(&staging)?;
    if !status.success() {
        bail!("tar exited {}", status.code().unwrap_or(-1));
    }
    let digest = hex(&Sha256::digest(std::fs::read(out.join(&archive))?));
    std::fs::write(out.join(format!("{archive}.sha256")), format!("{digest}  {archive}\n"))?;
    let sbom = sbom(root, profile, version, target)?;
    std::fs::write(out.join(format!("{stem}.cdx.json")), serde_json::to_string_pretty(&sbom)? + "\n")?;
    let metadata = serde_json::json!({
        "profile": profile,
        "target": target,
        "archive": archive,
        "sha256": digest,
        "sbom": format!("{stem}.cdx.json"),
    });
    std::fs::write(out.join(format!("{stem}.release.json")), serde_json::to_string_pretty(&metadata)? + "\n")?;
    println!("release: {archive} {digest}");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One package of a resolved graph: name, version, licence expression, and whether it is a
/// workspace member.
struct Component {
    name: String,
    version: String,
    license: String,
    local: bool,
}

/// The packages `profile` links on `target`: the binary's normal dependencies, procedural
/// macros excluded, as `cargo tree` resolves them for that feature set alone.
fn components(root: &Path, profile: &str, target: &str) -> Result<Vec<Component>> {
    let o = Command::new("cargo")
        .args(["tree", "--locked", "-p", PACKAGE, "--no-default-features", "--features", profile, "--target", target])
        .args(["-e", "normal,no-proc-macro", "--prefix", "none", "--format", "{p}|{l}"])
        .current_dir(root)
        .output()
        .context("running cargo tree")?;
    if !o.status.success() {
        bail!("cargo tree: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let line = line.trim().trim_end_matches("(*)").trim();
        let Some((package, license)) = line.split_once('|') else { continue };
        let mut words = package.split_whitespace();
        let (Some(name), Some(version)) = (words.next(), words.next()) else { continue };
        let version = version.trim_start_matches('v');
        if !seen.insert((name.to_string(), version.to_string())) {
            continue;
        }
        out.push(Component {
            name: name.to_string(),
            version: version.to_string(),
            license: license.trim().trim_end_matches("(*)").trim().to_string(),
            local: package.contains(" ("),
        });
    }
    Ok(out)
}

/// The CycloneDX 1.5 SBOM of `profile` built for `target`: the binary as the described
/// component and every other linked package as a library carrying its purl and licence.
fn sbom(root: &Path, profile: &str, version: &str, target: &str) -> Result<serde_json::Value> {
    let all = components(root, profile, target)?;
    let libraries: Vec<serde_json::Value> = all
        .iter()
        .filter(|c| c.name != PACKAGE)
        .map(|c| {
            let mut v = serde_json::json!({
                "type": "library",
                "name": c.name,
                "version": c.version,
                "purl": format!("pkg:cargo/{}@{}", c.name, c.version),
            });
            if !c.license.is_empty() {
                v["licenses"] = serde_json::json!([{ "expression": c.license }]);
            }
            if c.local {
                v["scope"] = serde_json::json!("required");
                v["description"] = serde_json::json!("workspace package");
            }
            v
        })
        .collect();
    Ok(serde_json::json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": profile,
                "version": version,
                "purl": format!("pkg:cargo/{PACKAGE}@{version}?features={profile}&target={target}"),
                "licenses": [{ "expression": "Apache-2.0" }],
            },
            "properties": [
                { "name": "contextful:profile", "value": profile },
                { "name": "contextful:target", "value": target },
            ],
        },
        "components": libraries,
    }))
}

/// Ruby class name of a formula: `contextful-edge` → `ContextfulEdge`.
fn class_name(formula: &str) -> String {
    formula
        .split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}

/// The Homebrew platform block a target selects.
fn platform(target: &str) -> Option<(&'static str, &'static str)> {
    let os = if DARWIN.contains(&target) { "on_macos" } else if LINUX.contains(&target) { "on_linux" } else { return None };
    let arch = if target.starts_with("aarch64") { "on_arm" } else { "on_intel" };
    Some((os, arch))
}

/// A formula named `formula` installing `profile` from the archives at `base_url`,
/// with each target's digest supplied by the release matrix.
fn formula(formula: &str, profile: &str, version: &str, targets: &[&str], base_url: &str, digests: &BTreeMap<(String, String), String>) -> Result<String> {
    let mut blocks: Vec<(&str, Vec<String>)> = Vec::new();
    for target in targets {
        let Some((os, arch)) = platform(target) else { continue };
        let archive = format!("{}.tar.gz", stem(profile, version, target));
        let digest = digests.get(&(profile.to_string(), target.to_string())).context("release matrix digest missing")?;
        let block = format!(
            "    {arch} do\n      url \"{}/{archive}\"\n      sha256 \"{digest}\"\n    end\n",
            base_url.trim_end_matches('/')
        );
        match blocks.iter_mut().find(|(o, _)| *o == os) {
            Some((_, b)) => b.push(block),
            None => blocks.push((os, vec![block])),
        }
    }
    let mut body = String::new();
    for (os, arches) in blocks {
        body.push_str(&format!("  {os} do\n{}  end\n", arches.concat()));
    }
    Ok(format!(
        "# Generated by `contextful-ci formula`.\nclass {} < Formula\n  desc \"Contextful, the {} profile\"\n  homepage \"https://github.com/fractalboxdev/contextful\"\n  version \"{version}\"\n  license \"Apache-2.0\"\n\n{body}\n  def install\n    bin.install \"{BINARY}\"\n  end\n\n  test do\n    assert_match \"{profile}\", shell_output(\"#{{bin}}/{BINARY} --version\")\n  end\nend\n",
        class_name(formula),
        short(profile),
    ))
}

/// The complete release matrix, loaded from local checksum files or a metadata manifest.
fn digests(dist: &Path, manifest: Option<&Path>, version: &str) -> Result<BTreeMap<(String, String), String>> {
    let expected: Vec<_> = matrix().into_iter().flat_map(|(profile, targets)| targets.into_iter().map(move |target| (profile.to_string(), target.to_string()))).collect();
    let mut found = BTreeMap::new();
    if let Some(path) = manifest {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text).context("parsing release manifest")?;
        let cells = value.as_array().context("release manifest is not a JSON array")?;
        for cell in cells {
            let string = |key| cell.get(key).and_then(serde_json::Value::as_str).with_context(|| format!("release cell has no `{key}` string"));
            let profile = string("profile")?;
            let target = string("target")?;
            let key = (profile.to_string(), target.to_string());
            if !expected.contains(&key) {
                bail!("release manifest names an unknown cell: {profile} {target}");
            }
            let stem = stem(profile, version, target);
            if string("archive")? != format!("{stem}.tar.gz") || string("sbom")? != format!("{stem}.cdx.json") {
                bail!("release manifest names wrong assets for {profile} {target}");
            }
            let digest = string("sha256")?;
            if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                bail!("release manifest has an invalid SHA-256 for {profile} {target}");
            }
            if found.insert(key, digest.to_string()).is_some() {
                bail!("release manifest repeats {profile} {target}");
            }
        }
    } else {
        for (profile, target) in &expected {
            let archive = format!("{}.tar.gz", stem(profile, version, target));
            let sum = dist.join(format!("{archive}.sha256"));
            let text = std::fs::read_to_string(&sum).with_context(|| format!("reading {}", sum.display()))?;
            let digest = text.split_whitespace().next().context("an empty checksum file")?;
            found.insert((profile.clone(), target.clone()), digest.to_string());
        }
    }
    for (profile, target) in expected {
        if !found.contains_key(&(profile.clone(), target.clone())) {
            bail!("release manifest omits {profile} {target}");
        }
    }
    Ok(found)
}

/// Write `Formula/<profile>.rb`, the bare full-profile formula, and `SHA256SUMS`.
/// A manifest supplies every cell digest without downloading release archives.
pub fn formulae(root: &Path, dist: &Path, manifest: Option<&Path>, base_url: &str) -> Result<Vec<PathBuf>> {
    let version = version(root)?;
    let digests = digests(dist, manifest, &version)?;
    let dir = dist.join("Formula");
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for (profile, targets) in matrix() {
        let text = formula(profile, profile, &version, &targets, base_url, &digests)?;
        let path = dir.join(format!("{profile}.rb"));
        std::fs::write(&path, text)?;
        written.push(path);
        if profile == DEFAULT_PROFILE {
            let path = dir.join(format!("{BINARY}.rb"));
            std::fs::write(&path, formula(BINARY, profile, &version, &targets, base_url, &digests)?)?;
            written.push(path);
        }
    }
    let mut sums = String::new();
    let mut archives: Vec<_> = digests.iter().map(|((profile, target), digest)| (format!("{}.tar.gz", stem(profile, &version, target)), digest)).collect();
    archives.sort_by(|a, b| a.0.cmp(&b.0));
    for (archive, digest) in archives {
        sums.push_str(&format!("{digest}  {archive}\n"));
    }
    std::fs::write(dist.join("SHA256SUMS"), sums)?;
    Ok(written)
}

/// The target no profile ships for, which the scheduled probe builds the edge profile for
/// (`assurance.build.wasi-probe`).
const WASI: &str = "wasm32-wasip2";
const WASI_PROFILE: &str = "contextful-edge";
/// The ledger entry the probe records under.
const WASI_ENTRY: &str = "edge-wasip2-footprint";
/// The zstd level the release archive compresses with.
const WASI_LEVEL: i32 = 19;

/// Build the edge profile for `wasm32-wasip2` and record its zstd-compressed size in MiB
/// under the ledger entry. A failed build records nothing and returns cleanly, so the
/// scheduled tier reads an absent figure and runs its next entry. The build directory is
/// reclaimed either way.
pub fn wasi_probe(root: &Path, target_dir: &Path) -> Result<()> {
    let args = cargo_build(WASI_PROFILE, WASI, Builder::Cargo);
    eprintln!("wasi-probe: cargo {}", args.join(" "));
    let built = Command::new("cargo").args(&args).env("CARGO_TARGET_DIR", target_dir).current_dir(root).output().context("running cargo")?;
    let result = if built.status.success() {
        let wasm = target_dir.join(WASI).join("release").join(format!("{BINARY}.wasm"));
        let bytes = std::fs::read(&wasm).with_context(|| format!("reading {}", wasm.display()))?;
        let compressed = zstd::bulk::compress(&bytes, WASI_LEVEL).context("compressing the artifact")?;
        let mib = compressed.len() as f64 / (1024.0 * 1024.0);
        contextful_eval::record::emit(WASI_ENTRY, mib, 1, 0);
        eprintln!("wasi-probe: {WASI_PROFILE} for {WASI} compresses to {mib:.2} MiB");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&built.stderr);
        let tail: Vec<&str> = stderr.lines().filter(|l| l.starts_with("error")).take(5).collect();
        eprintln!("wasi-probe: {WASI_PROFILE} does not build for {WASI}; recorded nothing\n{}", tail.join("\n"));
        Ok(())
    };
    let _ = std::fs::remove_dir_all(target_dir);
    result
}
