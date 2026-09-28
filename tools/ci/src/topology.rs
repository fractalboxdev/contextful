//! `contextful-ci topology` — the dependency rules of the `topology` contract, read off
//! `cargo metadata`: the domain crate's purity and dependency direction, the model-vendor
//! and script-runtime bans, and the run-path-to-read-path crate graph; and, per package
//! off `cargo tree`, the store adapter's engine-free write half and the external-assertion
//! stack outside the binary. The same walk holds every workspace package to
//! `assurance.build.licence-field`.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Command;

/// The domain crate (`topology.package.domain-crate`).
const DOMAIN: &str = "contextful-core";
const DOMAIN_MANIFEST: &str = "crates/contextful-core/Cargo.toml";

/// The licence every workspace package declares (`assurance.build.licence-field`).
const LICENCE: &str = "Apache-2.0";

/// Packages the domain crate reaches through no normal dependency, by what each is
/// (`topology.package.domain-impurity`). An entry ending `*` is a name prefix.
const IMPURE: [(&str, &str); 19] = [
    ("tokio", "an async runtime"),
    ("async-std", "an async runtime"),
    ("smol", "an async runtime"),
    ("async-executor", "an async runtime"),
    ("wasmtime", "a component host"),
    ("wasmer", "a component host"),
    ("reqwest", "an HTTP client"),
    ("hyper", "an HTTP client"),
    ("ureq", "an HTTP client"),
    ("isahc", "an HTTP client"),
    ("surf", "an HTTP client"),
    ("attohttpc", "an HTTP client"),
    ("curl", "an HTTP client"),
    ("arrow*", "a columnar-format implementation"),
    ("parquet", "a columnar-format implementation"),
    ("polars*", "a columnar-format implementation"),
    ("duckdb", "a columnar-format implementation"),
    ("datafusion", "a columnar-format implementation"),
    ("libduckdb-sys", "a columnar-format implementation"),
];

/// The store adapter, whose write half resolves no SQL engine with its `read` feature off
/// (`topology.package.store-write-engine-free`).
const STORE: &str = "contextful-context";
const STORE_READ_FEATURE: &str = "read";
const SQL_ENGINE: [&str; 2] = ["duckdb", "libduckdb-sys"];

/// Model-vendor SDKs no workspace crate declares (`topology.compose.vendor-sdk`).
const VENDOR_SDKS: [&str; 12] = [
    "async-openai",
    "openai",
    "openai-api-rs",
    "anthropic",
    "anthropic-sdk",
    "misanthropy",
    "clust",
    "google-generative-ai",
    "genai",
    "ollama-rs",
    "mistralai-client",
    "cohere-rust",
];

/// JavaScript runtimes no profile links (`topology.compose.script-runtime`).
const SCRIPT_RUNTIMES: [&str; 9] =
    ["deno_core", "deno_runtime", "v8", "rusty_v8", "boa_engine", "rquickjs", "quickjs*", "js-sandbox", "mozjs"];

/// Run-path and read-path crates of the crate map. The tree holds no crossing crate, so
/// every path from the first set to the second is an undeclared crossing
/// (`topology.compose.undeclared-crossing`).
const RUN_PATH: [&str; 4] = ["contextful-engine", "contextful-outbound", "contextful-wasm", "contextful-connectors"];
const READ_PATH: [&str; 5] = ["contextful-context", "contextful-memory", "contextful-sync", "contextful-agent", "contextful-eval"];

/// The binary crate, the one package that may link the exchange, and only while its
/// source reaches [`EXCHANGE_MODULE`] (`topology.package.exchange-optional`).
const BINARY: &str = "contextful-cli";

/// The path through which the binary wires `authority.exchange.surface`.
const EXCHANGE_MODULE: &str = "contextful_policy::exchange";

/// The external-assertion stack no other `crates/` package resolves
/// (`topology.package.exchange-optional`).
const EXCHANGE_STACK: [&str; 2] = ["jsonwebtoken", "rsa"];

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

struct Package {
    name: String,
    /// Path of the manifest relative to the workspace root.
    manifest: String,
    /// Workspace packages are the members under `crates/` or `tools/`.
    workspace: bool,
    /// The manifest's resolved `license` field.
    license: Option<String>,
    /// Declared dependencies: `(package name, kind, optional)`.
    declared: Vec<(String, Option<String>, bool)>,
    /// `features` table: feature name to its enabled entries.
    features: HashMap<String, Vec<String>>,
}

struct Edge {
    to: String,
    /// Normal, build or dev, as `cargo metadata` spells it (`None` is normal).
    kinds: Vec<Option<String>>,
}

struct Graph {
    packages: HashMap<String, Package>,
    /// Package id to its resolved dependency edges.
    edges: HashMap<String, Vec<Edge>>,
    /// Package id to the features resolved on it.
    enabled: HashMap<String, Vec<String>>,
}

impl Graph {
    fn load(root: &Path) -> Result<Graph> {
        let out = Command::new("cargo")
            .args(["metadata", "--format-version", "1", "-q"])
            .current_dir(root)
            .output()
            .context("running cargo metadata")?;
        if !out.status.success() {
            bail!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let meta: Value = serde_json::from_slice(&out.stdout).context("parsing cargo metadata")?;
        let ws_root = meta["workspace_root"].as_str().unwrap_or_default().to_string();
        let members: Vec<&str> = meta["workspace_members"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let mut packages = HashMap::new();
        for p in meta["packages"].as_array().into_iter().flatten() {
            let id = p["id"].as_str().unwrap_or_default().to_string();
            let manifest = p["manifest_path"].as_str().unwrap_or_default();
            let manifest = manifest.strip_prefix(&ws_root).unwrap_or(manifest).trim_start_matches(['/', '\\']).replace('\\', "/");
            let workspace = members.contains(&id.as_str()) && (manifest.starts_with("crates/") || manifest.starts_with("tools/"));
            let declared = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| {
                    (
                        d["name"].as_str().unwrap_or_default().to_string(),
                        d["kind"].as_str().map(str::to_string),
                        d["optional"].as_bool().unwrap_or(false),
                    )
                })
                .collect();
            let features = p["features"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| {
                            (k.clone(), v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let name = p["name"].as_str().unwrap_or_default().to_string();
            let license = p["license"].as_str().map(str::to_string);
            packages.insert(id, Package { name, manifest, workspace, license, declared, features });
        }
        let mut edges = HashMap::new();
        let mut enabled = HashMap::new();
        for n in meta["resolve"]["nodes"].as_array().into_iter().flatten() {
            let id = n["id"].as_str().unwrap_or_default().to_string();
            let es = n["deps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| Edge {
                    to: d["pkg"].as_str().unwrap_or_default().to_string(),
                    kinds: d["dep_kinds"].as_array().into_iter().flatten().map(|k| k["kind"].as_str().map(str::to_string)).collect(),
                })
                .collect();
            enabled.insert(id.clone(), n["features"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect());
            edges.insert(id, es);
        }
        Ok(Graph { packages, edges, enabled })
    }

    fn id_of(&self, name: &str) -> Option<&String> {
        self.packages.iter().find(|(_, p)| p.workspace && p.name == name).map(|(id, _)| id)
    }

    fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.packages.get(id).map_or(id, |p| p.name.as_str())
    }

    /// Every package reachable from `from` over edges of the admitted kinds, each with
    /// the id path that first reaches it.
    fn reach(&self, from: &str, build_too: bool) -> Vec<Vec<String>> {
        let mut seen: HashMap<String, Vec<String>> = HashMap::new();
        let mut queue = VecDeque::from([vec![from.to_string()]]);
        seen.insert(from.to_string(), vec![from.to_string()]);
        let mut paths = Vec::new();
        while let Some(path) = queue.pop_front() {
            let last = path.last().cloned().unwrap_or_default();
            for e in self.edges.get(&last).into_iter().flatten() {
                let admitted = e.kinds.iter().any(|k| k.is_none() || (build_too && k.as_deref() == Some("build")));
                if !admitted || seen.contains_key(&e.to) {
                    continue;
                }
                let mut next = path.clone();
                next.push(e.to.clone());
                seen.insert(e.to.clone(), next.clone());
                paths.push(next.clone());
                queue.push_back(next);
            }
        }
        paths
    }

    /// `a -> b (feature `f`) -> c`: each hop an optional dependency took names the
    /// enabled feature of its dependent that turned it on.
    fn describe(&self, path: &[String]) -> String {
        let mut out = self.name(&path[0]).to_string();
        for pair in path.windows(2) {
            let (from, to) = (&pair[0], self.name(&pair[1]));
            out.push_str(" -> ");
            out.push_str(to);
            let Some(p) = self.packages.get(from) else { continue };
            if !p.declared.iter().any(|(n, k, opt)| n == to && k.is_none() && *opt) {
                continue;
            }
            let on = self.enabled.get(from).cloned().unwrap_or_default();
            let feature = on.iter().find(|f| {
                p.features.get(*f).is_some_and(|entries| {
                    entries.iter().any(|e| e == &format!("dep:{to}") || e == to || e.starts_with(&format!("{to}/")))
                })
            });
            match feature {
                Some(f) => out.push_str(&format!(" (feature `{f}`)")),
                None => out.push_str(&format!(" (feature `{to}`)")),
            }
        }
        out
    }
}

/// The line of `manifest` declaring dependency `dep`.
fn manifest_line(root: &Path, manifest: &str, dep: &str) -> String {
    let text = std::fs::read_to_string(root.join(manifest)).unwrap_or_default();
    let line = text.lines().position(|l| {
        let t = l.trim_start();
        t.strip_prefix(dep).is_some_and(|rest| rest.trim_start().starts_with('=') || rest.starts_with('.'))
            || t.ends_with(&format!(".{dep}]"))
    });
    match line {
        Some(n) => format!("{manifest}:{}", n + 1),
        None => manifest.to_string(),
    }
}

/// Each normal-dependency path from `package` to a package named in `targets`, resolved
/// by `cargo tree` for `package` alone — with its default features, or with every feature
/// off when `features_off` — as `a -> b -> c`. `cargo metadata` unifies features across
/// the workspace, so a per-package graph comes from `cargo tree -p`.
fn tree_paths(root: &Path, package: &str, features_off: bool, targets: &[&str]) -> Result<Vec<String>> {
    let mut args = vec!["tree", "-q", "-p", package];
    if features_off {
        args.push("--no-default-features");
    }
    args.extend(["-e", "normal", "--prefix", "depth", "--format", "{p}"]);
    let out = Command::new("cargo").args(&args).current_dir(root).output().context("running cargo tree")?;
    if !out.status.success() {
        bail!("cargo tree -p {package}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut stack: Vec<String> = Vec::new();
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let rest = line.trim_start_matches(|c: char| c.is_ascii_digit());
        let Ok(depth) = line[..line.len() - rest.len()].parse::<usize>() else { continue };
        let name = rest.split_whitespace().next().unwrap_or_default().to_string();
        stack.truncate(depth);
        stack.push(name.clone());
        if depth > 0 && targets.contains(&name.as_str()) {
            let path = stack.join(" -> ");
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

/// Every finding as `(error, message)`.
fn findings(root: &Path, g: &Graph) -> Result<Vec<(&'static str, String)>> {
    let mut out = Vec::new();
    let mut workspace: Vec<(&String, &Package)> = g.packages.iter().filter(|(_, p)| p.workspace).collect();
    workspace.sort_by(|a, b| a.1.name.cmp(&b.1.name));

    if let Some(core) = g.id_of(DOMAIN) {
        let manifest = g.packages.get(core).map_or(DOMAIN_MANIFEST.to_string(), |p| p.manifest.clone());
        for (dep, _, _) in g.packages.get(core).map(|p| p.declared.as_slice()).unwrap_or_default() {
            if workspace.iter().any(|(_, p)| &p.name == dep && p.name != DOMAIN) {
                out.push((
                    "TopologyDependencyInversion",
                    format!("`{DOMAIN}` depends on adapter crate `{dep}` at {}", manifest_line(root, &manifest, dep)),
                ));
            }
        }
        for path in g.reach(core, false) {
            let Some(last) = path.last() else { continue };
            let name = g.name(last);
            if let Some((_, what)) = IMPURE.iter().find(|(p, _)| matches(p, name)) {
                out.push(("DomainCrateImpurity", format!("`{DOMAIN}` links `{name}`, {what}, through {}", g.describe(&path))));
            }
        }
    }

    for (id, p) in &workspace {
        match p.license.as_deref() {
            Some(LICENCE) => {}
            Some(other) => out.push((
                "PackageLicenceMissing",
                format!("`{}` declares licence `{other}`, not `{LICENCE}` ({})", p.name, p.manifest),
            )),
            None => out.push(("PackageLicenceMissing", format!("`{}` declares no licence ({})", p.name, p.manifest))),
        }
        for (dep, _, _) in &p.declared {
            if VENDOR_SDKS.contains(&dep.as_str()) {
                out.push(("VendorSdkLinked", format!("`{}` declares model-vendor SDK `{dep}` ({})", p.name, manifest_line(root, &p.manifest, dep))));
            }
        }
        if p.manifest.starts_with("crates/") {
            for path in g.reach(id, true) {
                let Some(last) = path.last() else { continue };
                let name = g.name(last);
                if SCRIPT_RUNTIMES.iter().any(|r| matches(r, name)) {
                    out.push(("ScriptRuntimeLinked", format!("`{}` links JavaScript runtime `{name}` through {}", p.name, g.describe(&path))));
                }
            }
        }
        if RUN_PATH.contains(&p.name.as_str()) {
            for path in g.reach(id, true) {
                let Some(last) = path.last() else { continue };
                let name = g.name(last);
                if READ_PATH.contains(&name) && g.packages.get(last).is_some_and(|q| q.workspace) {
                    out.push((
                        "CrateGraphViolation",
                        format!("run-path crate `{}` reaches read-path crate `{name}` through {}, and no crossing crate carries the edge", p.name, g.describe(&path)),
                    ));
                }
            }
        }
    }

    if g.id_of(STORE).is_some() {
        for path in tree_paths(root, STORE, true, &SQL_ENGINE)? {
            let name = path.rsplit(" -> ").next().unwrap_or_default();
            out.push(("StoreWriteLinksEngine", format!("`{STORE}` without `{STORE_READ_FEATURE}` links `{name}` through {path}")));
        }
    }
    Ok(out)
}

/// `ExchangeDependencyLeak` per `crates/` package whose own resolved normal graph reaches
/// the exchange stack, naming the first path; the binary is excepted while its source
/// names [`EXCHANGE_MODULE`].
fn exchange_leaks(root: &Path, g: &Graph) -> Result<Vec<(&'static str, String)>> {
    let mut packages: Vec<&Package> =
        g.packages.values().filter(|p| p.workspace && p.manifest.starts_with("crates/")).collect();
    packages.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    let mut out = Vec::new();
    for package in packages {
        let name = package.name.as_str();
        let src = Path::new(&package.manifest).parent().unwrap_or(Path::new("")).join("src");
        if name == BINARY && mentions(&root.join(&src), EXCHANGE_MODULE) {
            continue;
        }
        if let Some(path) = tree_paths(root, name, false, &EXCHANGE_STACK)?.into_iter().next() {
            let dep = path.rsplit(" -> ").next().unwrap_or_default();
            let unwired = if name == BINARY {
                format!(", and no source under {} names `{EXCHANGE_MODULE}`", src.display())
            } else {
                String::new()
            };
            out.push(("ExchangeDependencyLeak", format!("`{name}` links `{dep}` through {path}{unwired}")));
        }
    }
    Ok(out)
}

/// Whether any `.rs` file under `dir` contains `needle`.
fn mentions(dir: &Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| {
        let path = e.path();
        if path.is_dir() {
            mentions(&path, needle)
        } else {
            path.extension().is_some_and(|x| x == "rs")
                && std::fs::read_to_string(&path).is_ok_and(|text| text.contains(needle))
        }
    })
}

/// Run every topology dependency rule over the workspace at `root`.
pub fn check(root: &Path) -> Result<()> {
    let g = Graph::load(root)?;
    let mut found = findings(root, &g)?;
    found.extend(exchange_leaks(root, &g)?);
    for (code, message) in &found {
        eprintln!("{code}: {message}");
    }
    if let Some((code, _)) = found.first() {
        return Err(crate::refuse(code, format!("{} topology finding(s)", found.len())));
    }
    let n = g.packages.values().filter(|p| p.workspace).count();
    let domain = if g.id_of(DOMAIN).is_some() { format!("; `{DOMAIN}` is pure and depends on no adapter") } else { String::new() };
    println!("topology: {n} workspace package(s) hold to the dependency rules{domain}");
    Ok(())
}
