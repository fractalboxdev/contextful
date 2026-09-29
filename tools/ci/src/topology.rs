//! `contextful-ci topology` — the dependency rules of the `topology` contract, read off
//! `cargo metadata`: the domain crate's purity and dependency direction, the model-vendor
//! and script-runtime bans, and the run-path-to-read-path crate graph; and, per package
//! off `cargo tree`, the store adapter's engine-free write half, its SQLite-free graph,
//! the mediated-request crate's stack-free build without its transport feature, the
//! record decoders' network-free graph and the external-assertion stack outside the
//! binary; and off the manifests, the SQLite binding held to its one adapter package. The
//! same walk holds every workspace package to `assurance.build.licence-field`, and every
//! `crates/` package to the crate tree of `topology.package.crate-map-drift`.

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

/// The SQLite link-carrying package the store adapter resolves through no normal
/// dependency, on any target (`topology.package.store-sqlite-free`).
const SQLITE_SYS: &str = "libsqlite3-sys";

/// The store adapter, whose write half — its graph with the `read` feature off, across every
/// target — resolves no SQL engine, async runtime, HTTP or TLS stack, or SQLite
/// (`topology.package.store-write-engine-free`).
const STORE: &str = "contextful-context";
const STORE_READ_FEATURE: &str = "read";
const STORE_WRITE_DENY: [&str; 12] = [
    "duckdb",
    "libduckdb-sys",
    "tokio",
    "async-std",
    "smol",
    "async-executor",
    "ureq",
    "hyper",
    "reqwest",
    "rustls",
    "curl",
    SQLITE_SYS,
];

/// The one package declaring the SQLite binding, and the binding's packages
/// (`topology.package.sqlite-adapter`).
const SQLITE_ADAPTER: &str = "contextful-sqlite";
const SQLITE_BINDINGS: [&str; 2] = ["rusqlite", SQLITE_SYS];

/// The adapter's feature compiling SQLite into the build, which only the binary enables.
const SQLITE_BUNDLED: &str = "bundled";

/// Binding features choosing which SQLite build links. An entry ending `*` is a name prefix.
const SQLITE_LINK_FEATURES: [&str; 4] = ["bundled*", "sqlcipher", "in_gecko", "loadable_extension"];

/// The mediated-request crate, whose HTTP stack sits behind its default-on transport feature
/// (`topology.package.transport-optional`).
const RUNTIME: &str = "contextful-outbound";
const RUNTIME_TRANSPORT_FEATURE: &str = "transport-ureq";
const HTTP_STACK: [&str; 5] = ["ureq", "hyper", "reqwest", "rustls", "curl"];

/// The record-decoder package, and what it resolves through no normal dependency: the
/// mediated-request crate and the network stack (`topology.package.decode-network-free`).
const DECODE: &str = "contextful-decode";
const DECODE_BANNED: [&str; 7] = [RUNTIME, "ureq", "hyper", "reqwest", "rustls", "curl", "tokio"];

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

/// The page holding the crate-map clause and the crate tree under its `## Shapes`
/// (`topology.package.crate-map-drift`).
const TOPOLOGY_PAGE: &str = "spec/01-topology.md";
const CRATE_MAP_CLAUSE: &str = "- `crate-map` — ";

/// Count words the crate-map clause opens with.
const NUMBERS: [&str; 30] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
    "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty", "twenty-one",
    "twenty-two", "twenty-three", "twenty-four", "twenty-five", "twenty-six", "twenty-seven", "twenty-eight",
    "twenty-nine",
];

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
    /// Each declared dependency and the features its line turns on.
    enables: Vec<Declared>,
}

struct Declared {
    /// Package name.
    name: String,
    /// The key feature entries use: the rename, or the package name.
    key: String,
    /// Normal, build or dev (`None` is normal).
    kind: Option<String>,
    /// Features the declaration turns on, with `default` when its default features stay on.
    features: Vec<String>,
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
            .args(["metadata", "--format-version", "1", "-q", "--locked"])
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
            let enables = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| {
                    let name = d["name"].as_str().unwrap_or_default().to_string();
                    let mut features: Vec<String> =
                        d["features"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
                    if d["uses_default_features"].as_bool().unwrap_or(true) {
                        features.push("default".to_string());
                    }
                    Declared {
                        key: d["rename"].as_str().map_or_else(|| name.clone(), str::to_string),
                        name,
                        kind: d["kind"].as_str().map(str::to_string),
                        features,
                    }
                })
                .collect();
            let name = p["name"].as_str().unwrap_or_default().to_string();
            let license = p["license"].as_str().map(str::to_string);
            packages.insert(id, Package { name, manifest, workspace, license, declared, features, enables });
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

/// Which graph of one package `cargo tree` resolves.
#[derive(Clone, Copy)]
struct Resolve {
    /// Every feature off, where `false` keeps the default features.
    features_off: bool,
    /// Every target platform, where `false` resolves the host's alone.
    all_targets: bool,
}

const DEFAULT: Resolve = Resolve { features_off: false, all_targets: false };
const FEATURES_OFF: Resolve = Resolve { features_off: true, all_targets: false };
/// The store adapter's write half, resolved alike on every host.
const WRITE_HALF: Resolve = Resolve { features_off: true, all_targets: true };
/// Default features on every target, so a link behind another target's `cfg` resolves on
/// every host.
const DEFAULT_ALL_TARGETS: Resolve = Resolve { features_off: false, all_targets: true };

/// `cargo tree --locked` over the normal dependencies of `package` alone, one `<depth><name>`
/// line per node. `cargo metadata` unifies features across the workspace, so a per-package
/// graph comes from `cargo tree -p`.
fn tree(root: &Path, package: &str, resolve: Resolve) -> Result<String> {
    let mut args = vec!["tree", "-q", "--locked", "-p", package];
    if resolve.features_off {
        args.push("--no-default-features");
    }
    if resolve.all_targets {
        args.extend(["--target", "all"]);
    }
    args.extend(["-e", "normal", "--prefix", "depth", "--format", "{p}"]);
    let out = Command::new("cargo").args(&args).current_dir(root).output().context("running cargo tree")?;
    if !out.status.success() {
        bail!("cargo tree -p {package}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `(depth, package name)` of each line `cargo tree --prefix depth` printed.
fn nodes(tree: &str) -> impl Iterator<Item = (usize, String)> + '_ {
    tree.lines().filter_map(|line| {
        let rest = line.trim_start_matches(|c: char| c.is_ascii_digit());
        let depth = line[..line.len() - rest.len()].parse::<usize>().ok()?;
        Some((depth, rest.split_whitespace().next().unwrap_or_default().to_string()))
    })
}

/// The distinct package names in `package`'s graph under `resolve`, itself included.
fn tree_packages(root: &Path, package: &str, resolve: Resolve) -> Result<Vec<String>> {
    let mut names: Vec<String> = nodes(&tree(root, package, resolve)?).map(|(_, n)| n).collect();
    names.sort_unstable();
    names.dedup();
    Ok(names)
}

/// Each normal-dependency path from `package` to a package named in `targets` under
/// `resolve`, as `a -> b -> c`.
fn tree_paths(root: &Path, package: &str, resolve: Resolve, targets: &[&str]) -> Result<Vec<String>> {
    let text = tree(root, package, resolve)?;
    let mut stack: Vec<String> = Vec::new();
    let mut paths = Vec::new();
    for (depth, name) in nodes(&text) {
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
        for path in tree_paths(root, STORE, WRITE_HALF, &STORE_WRITE_DENY)? {
            let name = path.rsplit(" -> ").next().unwrap_or_default();
            out.push(("StoreWriteLinksEngine", format!("`{STORE}` without `{STORE_READ_FEATURE}` links `{name}` through {path}")));
        }
        for path in tree_paths(root, STORE, DEFAULT_ALL_TARGETS, &[SQLITE_SYS])? {
            out.push(("StoreLinksSqlite", format!("`{STORE}` links `{SQLITE_SYS}` through {path}")));
        }
    }
    if g.id_of(RUNTIME).is_some() {
        for path in tree_paths(root, RUNTIME, FEATURES_OFF, &HTTP_STACK)? {
            let name = path.rsplit(" -> ").next().unwrap_or_default();
            out.push(("TransportStackLinked", format!("`{RUNTIME}` without `{RUNTIME_TRANSPORT_FEATURE}` links `{name}` through {path}")));
        }
    }
    if g.id_of(DECODE).is_some() {
        for path in tree_paths(root, DECODE, DEFAULT, &DECODE_BANNED)? {
            let name = path.rsplit(" -> ").next().unwrap_or_default();
            out.push(("DecodeLinksNetwork", format!("`{DECODE}` links `{name}` through {path}")));
        }
    }
    out.extend(sqlite_link_forced(root, &workspace));
    Ok(out)
}

/// `SqliteLinkForced` per normal declaration of the SQLite binding outside the adapter, and
/// per package other than the binary whose dependency lines or feature table reach a choice
/// of SQLite build: the adapter's `bundled`, or a link feature of the binding. The adapter
/// is held to its own `bundled` forwarding alone. A feature chain is followed through the
/// package's own table; another workspace package's features answer for that package, so
/// each finding names the package that makes the choice (`topology.package.sqlite-adapter`).
fn sqlite_link_forced(root: &Path, workspace: &[(&String, &Package)]) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (_, p) in workspace {
        let adapter = p.name == SQLITE_ADAPTER;
        for d in p.enables.iter().filter(|d| d.kind.is_none()) {
            if SQLITE_BINDINGS.contains(&d.name.as_str()) && !adapter {
                let at = manifest_line(root, &p.manifest, &d.key);
                out.push(("SqliteLinkForced", format!("`{}` declares `{}` at {at}; only `{SQLITE_ADAPTER}` declares the SQLite binding", p.name, d.name)));
            }
        }
        if p.name == BINARY {
            continue;
        }
        let mut reported: Vec<(String, String)> = Vec::new();
        for d in p.enables.iter().filter(|d| d.kind.is_none()) {
            for f in d.features.iter().filter(|f| sqlite_choice(&d.name, f)) {
                let at = manifest_line(root, &p.manifest, &d.key);
                reported.push((d.name.clone(), f.clone()));
                out.push(("SqliteLinkForced", sqlite_message(p, &format!("{}/{f}", d.name), &format!("at {at}"))));
            }
        }
        let mut roots: Vec<&String> = p.features.keys().filter(|f| !(adapter && f.as_str() == SQLITE_BUNDLED)).collect();
        roots.sort_by_key(|f| (f.as_str() != "default", f.as_str()));
        for f in roots {
            for chain in feature_chains(p, f) {
                let Some((dep, feature)) = chain.last().cloned() else { continue };
                // A chain passing through a choice reports that choice, not what it forwards to.
                let through = chain[..chain.len() - 1].iter().any(|(q, g)| sqlite_choice(q, g));
                if through || !sqlite_choice(&dep, &feature) || reported.contains(&(dep.clone(), feature.clone())) {
                    continue;
                }
                reported.push((dep.clone(), feature.clone()));
                let shown = |(q, g): &(String, String)| if q == &p.name { g.clone() } else { format!("{q}/{g}") };
                let path = chain.iter().map(shown).collect::<Vec<_>>().join(" -> ");
                let how = if f == "default" { "by default".to_string() } else { format!("through feature `{f}`") };
                let at = feature_line(root, &p.manifest, f);
                out.push(("SqliteLinkForced", sqlite_message(p, &shown(&(dep, feature)), &format!("{how} ({path}, {at})"))));
            }
        }
    }
    out
}

/// Whether feature `feature` of package `dep` chooses the SQLite build: the adapter's
/// `bundled`, or a link feature of the binding.
fn sqlite_choice(dep: &str, feature: &str) -> bool {
    (dep == SQLITE_ADAPTER && feature == SQLITE_BUNDLED)
        || (SQLITE_BINDINGS.contains(&dep) && SQLITE_LINK_FEATURES.iter().any(|l| matches(l, feature)))
}

fn sqlite_message(p: &Package, target: &str, how: &str) -> String {
    if p.name == SQLITE_ADAPTER {
        format!("`{SQLITE_ADAPTER}` turns on `{target}` {how}; the host chooses the SQLite build")
    } else {
        format!("`{}` enables `{target}` {how}; only `{BINARY}` compiles SQLite in", p.name)
    }
}

/// Every `(package, feature)` feature `root` of `p` turns on, each as the chain of nodes that
/// first reaches it. The chain walks `p`'s own feature table; a node on another package ends
/// it. Turning on a dependency, by `dep:x` or an optional dependency's own name, turns on the
/// features its declaration lists.
fn feature_chains(p: &Package, root: &str) -> Vec<Vec<(String, String)>> {
    let dep_of = |key: &str| p.enables.iter().find(|d| d.kind.as_deref() != Some("dev") && d.key == key);
    let mut seen = vec![(p.name.clone(), root.to_string())];
    let mut queue = VecDeque::from([vec![(p.name.clone(), root.to_string())]]);
    let mut chains = Vec::new();
    while let Some(chain) = queue.pop_front() {
        let Some((q, f)) = chain.last().cloned() else { continue };
        if q != p.name {
            continue;
        }
        let mut next: Vec<(String, String)> = Vec::new();
        let activate = |key: &str, next: &mut Vec<(String, String)>| {
            if let Some(d) = dep_of(key) {
                next.extend(d.features.iter().map(|g| (d.name.clone(), g.clone())));
            }
        };
        for e in p.features.get(&f).into_iter().flatten() {
            if let Some(key) = e.strip_prefix("dep:") {
                activate(key, &mut next);
            } else if let Some((key, g)) = e.split_once('/') {
                let weak = key.ends_with('?');
                let key = key.trim_end_matches('?');
                if let Some(d) = dep_of(key) {
                    next.push((d.name.clone(), g.to_string()));
                }
                if !weak {
                    activate(key, &mut next);
                }
            } else if p.features.contains_key(e) {
                next.push((p.name.clone(), e.clone()));
            } else {
                activate(e, &mut next);
            }
        }
        for n in next {
            if seen.contains(&n) {
                continue;
            }
            seen.push(n.clone());
            let mut longer = chain.clone();
            longer.push(n);
            chains.push(longer.clone());
            queue.push_back(longer);
        }
    }
    chains
}

/// The line of `manifest` defining feature `feature` in its `[features]` table.
fn feature_line(root: &Path, manifest: &str, feature: &str) -> String {
    let text = std::fs::read_to_string(root.join(manifest)).unwrap_or_default();
    let mut table = false;
    for (n, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            table = t == "[features]";
        } else if table && t.strip_prefix(feature).is_some_and(|rest| rest.trim_start().starts_with('=')) {
            return format!("{manifest}:{}", n + 1);
        }
    }
    manifest.to_string()
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
        if let Some(path) = tree_paths(root, name, DEFAULT, &EXCHANGE_STACK)?.into_iter().next() {
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

/// The crate-map clause's stated count and the crate tree's entries, read off
/// [`TOPOLOGY_PAGE`]; `None` when the page is absent.
fn crate_map(root: &Path) -> Result<Option<(Option<usize>, Vec<String>)>> {
    let Ok(text) = std::fs::read_to_string(root.join(TOPOLOGY_PAGE)) else { return Ok(None) };
    let stated = text.lines().find_map(|l| l.strip_prefix(CRATE_MAP_CLAUSE)).map(|rest| {
        let word = rest.split_whitespace().next().unwrap_or_default().to_lowercase();
        NUMBERS.iter().position(|n| *n == word)
    });
    let Some(stated) = stated else { bail!("{TOPOLOGY_PAGE} holds no `crate-map` clause") };
    let mut entries = Vec::new();
    let mut in_shapes = false;
    let mut in_tree = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            in_shapes = line.trim_end() == "## Shapes";
            continue;
        }
        if !in_shapes {
            continue;
        }
        if line.trim_end() == "crates/" {
            in_tree = true;
        } else if in_tree && line.starts_with("  ") {
            if let Some(dir) = line.split_whitespace().next().and_then(|t| t.strip_suffix('/')) {
                entries.push(dir.to_string());
            }
        } else if in_tree {
            break;
        }
    }
    if entries.is_empty() {
        bail!("{TOPOLOGY_PAGE} holds no crate tree under `## Shapes`");
    }
    Ok(Some((stated, entries)))
}

/// `CrateMapDrift` per `crates/` package the crate tree omits, and for a stated count
/// differing from the tree's entries.
fn crate_map_drift(root: &Path, g: &Graph) -> Result<Vec<(&'static str, String)>> {
    let Some((stated, entries)) = crate_map(root)? else { return Ok(Vec::new()) };
    let mut dirs: Vec<&str> = g
        .packages
        .values()
        .filter(|p| p.workspace)
        .filter_map(|p| p.manifest.strip_prefix("crates/")?.strip_suffix("/Cargo.toml"))
        .collect();
    dirs.sort_unstable();
    let mut out = Vec::new();
    for dir in dirs {
        if !entries.iter().any(|e| e == dir) {
            out.push(("CrateMapDrift", format!("`crates/{dir}` is absent from the crate tree in {TOPOLOGY_PAGE}")));
        }
    }
    if stated != Some(entries.len()) {
        let stated = stated.map_or("no count".to_string(), |n| format!("{n} crates"));
        out.push((
            "CrateMapDrift",
            format!("`topology.package.crate-map` states {stated}; the crate tree lists {}", entries.len()),
        ));
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

/// Run every topology dependency rule over the workspace at `root`, each graph resolved
/// `--locked` so a `Cargo.lock` behind its manifests fails before any rule runs
/// (`assurance.gate.locked-resolve`). The store adapter's write half prints its distinct
/// package count and how many of them the write half is denied, which the ledger records.
pub fn check(root: &Path) -> Result<()> {
    let g = Graph::load(root)?;
    if g.id_of(STORE).is_some() {
        let names = tree_packages(root, STORE, WRITE_HALF)?;
        let forbidden = names.iter().filter(|n| STORE_WRITE_DENY.contains(&n.as_str())).count();
        println!("store write half: {} unique package(s), {forbidden} forbidden", names.len());
    }
    let mut found = findings(root, &g)?;
    found.extend(exchange_leaks(root, &g)?);
    found.extend(crate_map_drift(root, &g)?);
    for (code, message) in &found {
        eprintln!("{code}: {message}");
    }
    if let Some((code, _)) = found.first() {
        return Err(crate::refuse(code, format!("{} topology finding(s)", found.len())));
    }
    let n = g.packages.values().filter(|p| p.workspace).count();
    let domain = if g.id_of(DOMAIN).is_some() { format!("; `{DOMAIN}` is pure and depends on no adapter") } else { String::new() };
    println!("topology: {n} workspace package(s) hold to the dependency rules{domain}");
    if let Some((_, entries)) = crate_map(root)? {
        println!("crate map: {} crates, every `crates/` package named", entries.len());
    }
    Ok(())
}
