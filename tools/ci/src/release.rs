//! `contextful-ci release` (`assurance.build.targets`, `assurance.build.release-artifact`):
//! each profile built for a release target and packaged as an archive beside its SHA-256
//! checksum and a CycloneDX SBOM; `contextful-ci formula` writes the package-manager
//! formulae over a directory of them.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The binary every profile builds.
const BINARY: &str = "contextful";
const PACKAGE: &str = "contextful-cli";
/// The directory release builds compile into, apart from every gate stage's.
pub const TARGET_DIR: &str = "target/release-artifacts";

const LINUX: [&str; 2] = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];
const DARWIN: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// The profile the bare formula name and the install script resolve to.
pub const DEFAULT_PROFILE: &str = "contextful-full";

/// Every profile with the targets it ships for: all three cross-compile to Linux on musl,
/// and edge and full also build for macOS.
pub fn matrix() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("contextful-control", LINUX.to_vec()),
        ("contextful-edge", LINUX.iter().chain(DARWIN.iter()).copied().collect()),
        ("contextful-full", LINUX.iter().chain(DARWIN.iter()).copied().collect()),
    ]
}

/// The release target this host builds natively: musl on Linux, the Apple target on macOS.
pub fn host_target() -> Result<String> {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "linux" => Ok(format!("{arch}-unknown-linux-musl")),
        "macos" => Ok(format!("{arch}-apple-darwin")),
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
fn cargo_build(profile: &str, target: &str) -> Vec<String> {
    ["build", "--release", "--locked", "-p", PACKAGE, "--bin", BINARY, "--no-default-features", "--features", profile, "--target", target]
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
pub fn release(root: &Path, profiles: &[String], targets: &[String], target_dir: &Path, out: &Path) -> Result<()> {
    let targets = if targets.is_empty() { vec![host_target()?] } else { targets.to_vec() };
    let selected = cells(profiles, &targets)?;
    if selected.is_empty() {
        bail!("no profile ships for {}", targets.join(", "));
    }
    let version = version(root)?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    for (profile, target) in selected {
        let args = cargo_build(profile, target);
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
        let binary = target_dir.join(target).join("release").join(BINARY);
        package(root, profile, &version, target, &binary, out)?;
    }
    Ok(())
}

/// Write `<stem>.tar.gz` holding `binary` as `contextful` and the licence, `<stem>.tar.gz.sha256`
/// in `sha256sum` form, and `<stem>.cdx.json`, the SBOM of the profile's resolved graph.
fn package(root: &Path, profile: &str, version: &str, target: &str, binary: &Path, out: &Path) -> Result<()> {
    let stem = stem(profile, version, target);
    let staging = out.join(&stem);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    std::fs::copy(binary, staging.join(BINARY)).with_context(|| format!("copying {}", binary.display()))?;
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
fn platform(target: &str) -> (&'static str, &'static str) {
    let os = if target.ends_with("apple-darwin") { "on_macos" } else { "on_linux" };
    let arch = if target.starts_with("aarch64") { "on_arm" } else { "on_intel" };
    (os, arch)
}

/// A formula named `formula` installing `profile` from the archives under `dist`, whose
/// checksum files supply each target's digest, fetched from `base_url`.
fn formula(dist: &Path, formula: &str, profile: &str, version: &str, targets: &[&str], base_url: &str) -> Result<String> {
    let mut blocks: Vec<(&str, Vec<String>)> = Vec::new();
    for target in targets {
        let archive = format!("{}.tar.gz", stem(profile, version, target));
        let sum = dist.join(format!("{archive}.sha256"));
        let text = std::fs::read_to_string(&sum).with_context(|| format!("reading {}", sum.display()))?;
        let digest = text.split_whitespace().next().context("an empty checksum file")?.to_string();
        let (os, arch) = platform(target);
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

/// Write `Formula/<profile>.rb` for every profile with an archive for each of its targets
/// under `dist`, and `Formula/contextful.rb`, the bare name, resolving to the full profile.
pub fn formulae(root: &Path, dist: &Path, base_url: &str) -> Result<Vec<PathBuf>> {
    let version = version(root)?;
    let dir = dist.join("Formula");
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for (profile, targets) in matrix() {
        let text = formula(dist, profile, profile, &version, &targets, base_url)?;
        let path = dir.join(format!("{profile}.rb"));
        std::fs::write(&path, text)?;
        written.push(path);
        if profile == DEFAULT_PROFILE {
            let path = dir.join(format!("{BINARY}.rb"));
            std::fs::write(&path, formula(dist, BINARY, profile, &version, &targets, base_url)?)?;
            written.push(path);
        }
    }
    let mut sums = String::new();
    let mut names: Vec<_> = std::fs::read_dir(dist)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    for n in names.iter().filter(|n| n.ends_with(".tar.gz.sha256")) {
        sums.push_str(&std::fs::read_to_string(dist.join(n))?);
    }
    std::fs::write(dist.join("SHA256SUMS"), sums)?;
    Ok(written)
}
