//! `contextful-ci deny` — the profile-wide dependency refusals as cargo-deny bans
//! (`assurance.gate.dependency-deny`): `deny.toml` held to the refusals' crates
//! (`assurance.gate.deny-list`), and `cargo deny --locked check bans` run once per profile
//! feature of the binary (`assurance.gate.deny-profile-graph`), each hit reported under the
//! owning clause's error with the profile and the path that pulled it.

use crate::topology::{BINARY, SCRIPT_RUNTIMES, VENDOR_SDKS};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The deny list, at the workspace root.
const DENY_FILE: &str = "deny.toml";

/// The profiles, each a feature of the binary (`topology.package.profile`).
const PROFILES: [&str; 3] = ["contextful-edge", "contextful-full", "contextful-control"];

/// The CRDT library, which the control profile alone links (`topology.package.crdt-leak`).
const CRDT_LIBRARY: &str = "loro";

/// Backtracking regular-expression engines (`assurance.build.dependency-allowlist`).
const BACKTRACKING_MATCHERS: [&str; 3] = ["fancy-regex", "pcre2", "onig"];

/// Features turning a parser's depth limit off (`assurance.build.dependency-allowlist`).
const UNBOUNDED_FEATURES: [(&str, &str); 1] = [("serde_json", "unbounded_depth")];

/// The cargo-deny release the stage runs, and the SHA-256 of each host's archive.
const CARGO_DENY_VERSION: &str = "0.20.2";
const CARGO_DENY_ARCHIVES: [(&str, &str); 4] = [
    ("aarch64-apple-darwin", "fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3"),
    ("x86_64-apple-darwin", "248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6"),
    ("aarch64-unknown-linux-musl", "995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d"),
    ("x86_64-unknown-linux-musl", "9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"),
];
const CARGO_DENY_RELEASES: &str = "https://github.com/EmbarkStudios/cargo-deny/releases/download";

/// One profile-wide dependency refusal: the crates it denies, its clause and error, and the
/// profiles admitting them.
struct Refusal {
    clause: &'static str,
    error: &'static str,
    crates: Vec<&'static str>,
    admitted: &'static [&'static str],
}

fn refusals() -> Vec<Refusal> {
    vec![
        Refusal { clause: "topology.compose.vendor-sdk", error: "VendorSdkLinked", crates: VENDOR_SDKS.to_vec(), admitted: &[] },
        Refusal {
            clause: "topology.compose.script-runtime",
            error: "ScriptRuntimeLinked",
            // A prefix entry stays with `contextful-ci topology`, since a ban names one crate.
            crates: SCRIPT_RUNTIMES.iter().copied().filter(|r| !r.ends_with('*')).collect(),
            admitted: &[],
        },
        Refusal { clause: "topology.package.crdt-leak", error: "ProfileDependencyLeak", crates: vec![CRDT_LIBRARY], admitted: &["contextful-control"] },
        Refusal {
            clause: "assurance.build.dependency-allowlist",
            error: "DependencyAllowlistViolation",
            crates: BACKTRACKING_MATCHERS.to_vec(),
            admitted: &[],
        },
    ]
}

const FEATURE_ERROR: &str = "DependencyAllowlistViolation";
const FEATURE_CLAUSE: &str = "assurance.build.dependency-allowlist";

/// `DenyListDrift` per entry `deny.toml` lacks or carries beyond the refusals.
fn drift(config: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    let bans = config.get("bans");
    let listed: BTreeSet<(String, String)> = bans
        .and_then(|b| b.get("deny"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .map(|e| match e {
            toml::Value::String(name) => (name.clone(), String::new()),
            _ => (
                e.get("crate").and_then(toml::Value::as_str).unwrap_or_default().to_string(),
                e.get("reason").and_then(toml::Value::as_str).unwrap_or_default().to_string(),
            ),
        })
        .collect();
    let wanted: BTreeSet<(String, String)> =
        refusals().iter().flat_map(|r| r.crates.iter().map(|c| (c.to_string(), r.clause.to_string()))).collect();
    for (name, clause) in wanted.difference(&listed) {
        out.push(format!("`{DENY_FILE}` denies no `{name}` for `{clause}`"));
    }
    for (name, reason) in listed.difference(&wanted) {
        if wanted.iter().any(|(n, _)| n == name) {
            out.push(format!("`{DENY_FILE}` denies `{name}` for `{reason}`, which names no refusal of it"));
        } else {
            out.push(format!("`{DENY_FILE}` denies `{name}`, which no profile-wide dependency refusal names"));
        }
    }
    let features: BTreeSet<(String, String)> = bans
        .and_then(|b| b.get("features"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|e| {
            let name = e.get("crate").and_then(toml::Value::as_str).unwrap_or_default().to_string();
            let denied: Vec<String> =
                e.get("deny").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).map(str::to_string).collect();
            denied.into_iter().map(move |f| (name.clone(), f))
        })
        .collect();
    let wanted: BTreeSet<(String, String)> = UNBOUNDED_FEATURES.iter().map(|(c, f)| (c.to_string(), f.to_string())).collect();
    for (name, feature) in wanted.difference(&features) {
        out.push(format!("`{DENY_FILE}` denies no `{name}` feature `{feature}` for `{FEATURE_CLAUSE}`"));
    }
    for (name, feature) in features.difference(&wanted) {
        out.push(format!("`{DENY_FILE}` denies `{name}` feature `{feature}`, which no profile-wide dependency refusal names"));
    }
    out
}

/// `deny.toml` with the entries `profile` admits removed.
fn profile_config(config: &toml::Value, profile: &str) -> toml::Value {
    let admitted: HashSet<&str> =
        refusals().iter().filter(|r| r.admitted.contains(&profile)).flat_map(|r| r.crates.clone()).collect();
    let mut config = config.clone();
    if let Some(deny) = config.get_mut("bans").and_then(|b| b.get_mut("deny")).and_then(toml::Value::as_array_mut) {
        deny.retain(|e| {
            let name = e.as_str().or_else(|| e.get("crate").and_then(toml::Value::as_str)).unwrap_or_default();
            !admitted.contains(name)
        });
    }
    config
}

/// The binary's manifest and the features it declares, off `cargo metadata --no-deps`.
fn binary(root: &Path) -> Result<Option<(PathBuf, Vec<String>)>> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps", "-q", "--locked"])
        .current_dir(root)
        .output()
        .context("running cargo metadata")?;
    if !out.status.success() {
        bail!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let meta: Value = serde_json::from_slice(&out.stdout).context("parsing cargo metadata")?;
    Ok(meta["packages"].as_array().into_iter().flatten().find(|p| p["name"] == BINARY).map(|p| {
        let features = p["features"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        (PathBuf::from(p["manifest_path"].as_str().unwrap_or_default()), features)
    }))
}

/// The release target naming this host's cargo-deny archive.
fn host_target() -> Result<&'static str> {
    Ok(match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "linux") => "aarch64-unknown-linux-musl",
        ("x86_64", "linux") => "x86_64-unknown-linux-musl",
        (arch, os) => bail!("no cargo-deny {CARGO_DENY_VERSION} release archive is pinned for {arch}-{os}"),
    })
}

/// The pinned cargo-deny: one on `PATH` reporting the pinned version, or the release
/// archive for this host, fetched once into the user cache and verified by SHA-256 before
/// it unpacks. Concurrent runs each unpack into a scratch directory and rename it into place.
fn provision() -> Result<PathBuf> {
    let wanted = format!("cargo-deny {CARGO_DENY_VERSION}");
    if let Ok(o) = Command::new("cargo-deny").arg("--version").output() {
        if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == wanted {
            return Ok(PathBuf::from("cargo-deny"));
        }
    }
    let target = host_target()?;
    let digest = CARGO_DENY_ARCHIVES.iter().find(|(t, _)| *t == target).map(|(_, d)| *d).context("no pinned digest")?;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("neither XDG_CACHE_HOME nor HOME is set")?
        .join("contextful");
    let name = format!("cargo-deny-{CARGO_DENY_VERSION}-{target}");
    let dir = cache.join(&name);
    let bin = dir.join("cargo-deny");
    if bin.exists() {
        return Ok(bin);
    }
    std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
    let scratch = cache.join(format!(".{name}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;
    let archive = scratch.join(format!("{name}.tar.gz"));
    eprintln!("cargo-deny: fetching {CARGO_DENY_VERSION} for {target}");
    let url = format!("{CARGO_DENY_RELEASES}/{CARGO_DENY_VERSION}/{name}.tar.gz");
    let status = Command::new("curl").args(["-sSfL", "-o"]).arg(&archive).arg(&url).status().context("running curl")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&scratch);
        bail!("fetching {url} exited {}", status.code().unwrap_or(-1));
    }
    let bytes = std::fs::read(&archive)?;
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got != digest {
        let _ = std::fs::remove_dir_all(&scratch);
        bail!("{url} has SHA-256 {got}, not the pinned {digest}");
    }
    let status = Command::new("tar").arg("xzf").arg(&archive).arg("-C").arg(&scratch).status().context("running tar")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&scratch);
        bail!("unpacking {} exited {}", archive.display(), status.code().unwrap_or(-1));
    }
    // A concurrent run that renamed first leaves its copy in place; this one is discarded.
    if std::fs::rename(scratch.join(&name), &dir).is_err() && !bin.exists() {
        let _ = std::fs::remove_dir_all(&scratch);
        bail!("placing cargo-deny at {}", dir.display());
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(bin)
}

/// The key a graph node is indexed under: a crate's name and version, or a feature.
fn node_key(node: &Value) -> String {
    if let Some(k) = node.get("Krate") {
        format!("{}@{}", k["name"].as_str().unwrap_or_default(), k["version"].as_str().unwrap_or_default())
    } else if let Some(f) = node.get("Feature") {
        format!("{}/{}", f["crate_name"].as_str().unwrap_or_default(), f["name"].as_str().unwrap_or_default())
    } else {
        String::new()
    }
}

/// Every expanded node's parents, so a node printed as a repeat resolves to its first print.
fn index<'a>(node: &'a Value, into: &mut HashMap<String, &'a Vec<Value>>) {
    if let Some(parents) = node["parents"].as_array() {
        into.entry(node_key(node)).or_insert(parents);
        parents.iter().for_each(|p| index(p, into));
    }
}

/// `root -> ... -> crate`: one inclusion path of a diagnostic's inverted graph, crates only.
fn inclusion_path(graph: &Value) -> String {
    let mut parents_of = HashMap::new();
    index(graph, &mut parents_of);
    let mut seen = HashSet::new();
    let mut path = Vec::new();
    let mut node = graph;
    loop {
        let key = node_key(node);
        if !seen.insert(key.clone()) {
            break;
        }
        if let Some(k) = node.get("Krate") {
            let name = k["name"].as_str().unwrap_or_default().to_string();
            if path.last() != Some(&name) {
                path.push(name);
            }
        }
        let Some(parents) = parents_of.get(&key) else { break };
        let Some(next) = parents.iter().find(|p| !seen.contains(&node_key(p))) else { break };
        node = next;
    }
    path.reverse();
    path.join(" -> ")
}

/// Run cargo-deny's bans over `profile`'s graph, returning each hit as `(error, message)`.
fn check_profile(cargo_deny: &Path, manifest: &Path, config: &toml::Value, profile: &str) -> Result<Vec<(&'static str, String)>> {
    let file = std::env::temp_dir().join(format!("contextful-deny-{}-{profile}.toml", std::process::id()));
    std::fs::write(&file, toml::to_string(&profile_config(config, profile))?)?;
    let out = Command::new(cargo_deny)
        .args(["--locked", "--format", "json", "--color", "never", "--exclude-dev", "--no-default-features", "--features", profile])
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--config")
        .arg(&file)
        .args(["check", "bans"])
        .output()
        .context("running cargo-deny")?;
    let _ = std::fs::remove_file(&file);
    let refusals = refusals();
    let mut hits = Vec::new();
    let mut faults = Vec::new();
    for line in String::from_utf8_lossy(&out.stderr).lines().chain(String::from_utf8_lossy(&out.stdout).lines()) {
        let Ok(d) = serde_json::from_str::<Value>(line) else { continue };
        let f = &d["fields"];
        let error_level = f["severity"] == "error" || f["level"] == "ERROR";
        let graph = f["graphs"].get(0);
        let krate = graph.and_then(|g| g["Krate"]["name"].as_str()).unwrap_or_default();
        let path = graph.map(inclusion_path).unwrap_or_default();
        match f["code"].as_str() {
            Some("banned") => {
                let refusal = refusals.iter().find(|r| r.crates.contains(&krate));
                let error = refusal.map_or("DenyListDrift", |r| r.error);
                hits.push((error, format!("profile `{profile}` links `{krate}` through {path}")));
            }
            Some("feature-banned") => {
                let feature = f["labels"].as_array().into_iter().flatten().find_map(|l| l["span"].as_str()).unwrap_or_default();
                hits.push((FEATURE_ERROR, format!("profile `{profile}` turns on `{krate}` feature `{feature}` through {path}")));
            }
            _ if error_level => faults.push(f["message"].as_str().unwrap_or(line).to_string()),
            _ => {}
        }
    }
    if hits.is_empty() && !out.status.success() {
        let detail = if faults.is_empty() { String::from_utf8_lossy(&out.stderr).trim().to_string() } else { faults.join("; ") };
        bail!("cargo-deny over profile `{profile}` exited {}: {detail}", out.status.code().unwrap_or(-1));
    }
    Ok(hits)
}

/// Hold `deny.toml` to the refusals, then every profile the binary declares to it. A
/// workspace without the binary has no profile graph and passes untouched.
pub fn check(root: &Path) -> Result<()> {
    let Some((manifest, features)) = binary(root)? else {
        println!("dependency deny: no `{BINARY}` package, so no profile graph");
        return Ok(());
    };
    let config = match std::fs::read_to_string(root.join(DENY_FILE)) {
        Ok(text) => text.parse::<toml::Value>().with_context(|| format!("parsing {DENY_FILE}"))?,
        Err(_) => return Err(crate::refuse("DenyListDrift", format!("no `{DENY_FILE}` at the workspace root"))),
    };
    let drifted = drift(&config);
    if !drifted.is_empty() {
        for d in &drifted {
            eprintln!("DenyListDrift: {d}");
        }
        return Err(crate::refuse("DenyListDrift", format!("{} deny-list finding(s)", drifted.len())));
    }
    let cargo_deny = provision()?;
    let mut found = Vec::new();
    for profile in PROFILES {
        if !features.iter().any(|f| f == profile) {
            println!("dependency deny: the binary declares no `{profile}` feature; profile `{profile}` has no graph");
            continue;
        }
        let hits = check_profile(&cargo_deny, &manifest, &config, profile)?;
        if hits.is_empty() {
            println!("dependency deny: profile `{profile}` holds to `{DENY_FILE}`");
        }
        found.extend(hits);
    }
    for (code, message) in &found {
        eprintln!("{code}: {message}");
    }
    if let Some((code, _)) = found.first() {
        return Err(crate::refuse(code, format!("{} dependency-deny finding(s)", found.len())));
    }
    Ok(())
}
