//! `assurance.gate.affected-*`: the required parts a change reaches, measured from the merge
//! base. A path or pin the rules cannot place reaches every part.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

const LOCK: &str = "spec/spec.lock.json";
const PINS: &str = "spec/pins.toml";

/// The parts every listing holds (`assurance.gate.affected-parts`).
const ALWAYS: [&str; 3] = ["pins", "toolchain", "schema"];

/// A required part, or every part sharing a prefix.
enum Match {
    Part(String),
    Prefix(&'static str),
}

impl Match {
    fn hits(&self, part: &str) -> bool {
        match self {
            Match::Part(p) => p == part,
            Match::Prefix(p) => part.starts_with(p),
        }
    }
}

/// What one changed path or one pin reaches; `None` is every part.
type Reach = Option<Vec<Match>>;

fn part(p: &str) -> Match {
    Match::Part(p.to_string())
}

/// The formal stage and every CLI differential part: the parts running a pinned theorem.
fn formal() -> Vec<Match> {
    vec![part("formal"), part("workspace.cli-formal"), Match::Prefix("features.formal-")]
}

/// Every part reading the Lean tree under `formal/` (`assurance.gate.affected-formal`): the
/// formal stage, the CLI's `formal::` and differential suites, the foundation package that
/// builds `formal/reference`, the evaluate stage's protocol differential entry, and every
/// feature part re-running those suites.
fn lean_tree() -> Vec<Match> {
    vec![
        part("formal"),
        part("evaluate"),
        part("workspace.cli"),
        part("workspace.cli-formal"),
        part("workspace.foundation"),
        Match::Prefix("features."),
    ]
}

/// What a changed path under `spec/` or `references/` reaches (`assurance.gate.affected-spec`,
/// `-spec-inputs`): test-first validation; for `spec/`, the foundation part, whose corpus and
/// store suites read the live spec; and the stage reading the file directly.
fn reach_of_spec(path: &str) -> Vec<Match> {
    let mut m = vec![part("test-first.validate")];
    if path.starts_with("spec/") {
        m.push(part("workspace.foundation"));
    }
    match path {
        "spec/01-topology.md" | "spec/capabilities.toml" => m.push(part("crate-graph")),
        "spec/adr/A-assurance.md" => m.push(part("formal")),
        "spec/terms/assurance.toml" => m.push(Match::Prefix("budget.")),
        _ => {}
    }
    m
}

/// The subset of `required`, in its order, that the change HEAD against the merge base of
/// `base` reaches.
pub(crate) fn select(root: &Path, base: &str, required: Vec<String>) -> Result<Vec<String>> {
    let merge_base = crate::merge_base(base)?;
    let mut matches: Vec<Match> = ALWAYS.iter().map(|p| part(p)).collect();
    let mut spec = false;
    for path in changed_paths(&merge_base)? {
        let reach = if path.starts_with("spec/") || path.starts_with("references/") {
            spec = true;
            Some(reach_of_spec(&path))
        } else {
            reach_of_path(&path)
        };
        match reach {
            Some(m) => matches.extend(m),
            None => {
                eprintln!("affected: `{path}` reaches every part");
                return Ok(required);
            }
        }
    }
    if spec {
        let mut packages = Packages::default();
        match pinned(root, &merge_base, &mut packages)? {
            Some(m) => matches.extend(m),
            None => return Ok(required),
        }
    }
    Ok(required.into_iter().filter(|p| matches.iter().any(|m| m.hits(p))).collect())
}

/// What a changed path outside `spec/` and `references/` reaches
/// (`assurance.gate.affected-surfaces`, `-formal`, `-every`). A TypeScript path also reaches
/// acceptance, which builds `apps/console`, and crate-graph, which scans scripts for
/// capability homes.
fn reach_of_path(path: &str) -> Reach {
    let build_input = path == "Cargo.lock"
        || path == "Cargo.toml"
        || path.ends_with("/Cargo.toml")
        || path.starts_with("rust-toolchain")
        || (path.starts_with("formal/") && (path == "formal/lean-toolchain" || path.ends_with("/lean-toolchain")))
        || ["crates/", "tools/", ".github/"].iter().any(|dir| path.starts_with(dir));
    if build_input {
        None
    } else if path.starts_with("apps/") || path.starts_with("packages/") {
        Some(vec![part("surfaces"), part("acceptance"), part("crate-graph")])
    } else if path.starts_with("formal/") {
        Some(lean_tree())
    } else {
        None
    }
}

/// Every path the change adds, removes or modifies; a rename lists both sides.
fn changed_paths(merge_base: &str) -> Result<Vec<String>> {
    let out = crate::git(&["-c", "core.quotepath=off", "diff", "--name-only", "--no-renames", "-z", merge_base, "HEAD"])?;
    Ok(out.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
}

/// Workspace packages, read once and only when a Rust pin needs them.
#[derive(Default)]
struct Packages {
    read: Option<(BTreeSet<String>, BTreeSet<String>)>,
}

impl Packages {
    /// The parts running `package`'s tests; `None` for a package the workspace does not hold.
    fn reach(&mut self, root: &Path, package: &str) -> Result<Reach> {
        if self.read.is_none() {
            let names = crate::workspace_packages(root)?.into_iter().collect();
            let featured = crate::featured_packages(root)?.into_iter().map(|f| f.name).collect();
            self.read = Some((names, featured));
        }
        let (names, featured) = self.read.as_ref().expect("read above");
        if package == crate::ACCEPTANCE_PACKAGE {
            return Ok(Some(vec![part("acceptance")]));
        }
        if !names.contains(package) {
            return Ok(None);
        }
        let mut m = vec![part("workspace.compile"), Match::Part(format!("workspace.{}", crate::workspace_part(package)))];
        if package == crate::topology::BINARY {
            m.extend([part("workspace.cli-formal"), Match::Prefix("features.binary-"), Match::Prefix("features.formal-")]);
        } else if featured.contains(package) {
            m.push(part("features.packages"));
        }
        if crate::stage::CONNECTOR_PACKAGES.contains(&package) {
            m.push(part("connectors"));
        }
        Ok(Some(m))
    }
}

/// The parts running the pinned test or theorem of every clause whose lock statement or pins
/// entry differs between the merge base and HEAD (`assurance.gate.affected-spec`).
fn pinned(root: &Path, merge_base: &str, packages: &mut Packages) -> Result<Reach> {
    let (Some(before), Some(after)) = (clauses(merge_base), clauses("HEAD")) else {
        eprintln!("affected: `{LOCK}` is unreadable at the merge base or HEAD, so every part is reached");
        return Ok(None);
    };
    let (Some(pins_before), Some(pins_after)) = (pins(merge_base), pins("HEAD")) else {
        eprintln!("affected: `{PINS}` is unreadable at the merge base or HEAD, so every part is reached");
        return Ok(None);
    };
    let mut ids: BTreeSet<&String> = BTreeSet::new();
    for id in before.keys().chain(after.keys()) {
        if before.get(id) != after.get(id) {
            ids.insert(id);
        }
    }
    for id in pins_before.keys().chain(pins_after.keys()) {
        if pins_before.get(id) != pins_after.get(id) {
            ids.insert(id);
        }
    }
    let mut out = Vec::new();
    // The evaluate stage refuses a ledger entry naming a clause the lock lacks
    // (`assurance.gate.affected-removed`).
    if before.keys().any(|id| !after.contains_key(id)) {
        out.push(part("evaluate"));
    }
    if ids.is_empty() {
        return Ok(Some(out));
    }
    let tags = tags()?;
    for id in ids {
        for path in tags.get(id).into_iter().flatten() {
            match reach_of_pinned_file(root, path, packages)? {
                Some(m) => out.extend(m),
                None => return every(id, path),
            }
        }
        for pin in [pins_before.get(id), pins_after.get(id)].into_iter().flatten() {
            match reach_of_pin(root, pin, packages)? {
                Some(m) => out.extend(m),
                None => return every(id, &pin.to_string()),
            }
        }
    }
    Ok(Some(out))
}

fn every(id: &str, pin: &str) -> Result<Reach> {
    eprintln!("affected: clause `{id}` is pinned to `{pin}`, which reaches every part");
    Ok(None)
}

/// The file at `rev:path`, or `None` when it is absent there.
fn show(rev: &str, path: &str) -> Option<String> {
    let out = Command::new("git").args(["show", &format!("{rev}:{path}")]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Each clause's kind and statement at `rev`; an absent lock holds none, an unreadable one is `None`.
fn clauses(rev: &str) -> Option<BTreeMap<String, (String, String)>> {
    let Some(text) = show(rev, LOCK) else { return Some(BTreeMap::new()) };
    let lock: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut out = BTreeMap::new();
    for c in lock["clauses"].as_array()? {
        let field = |k: &str| c[k].as_str().unwrap_or_default().to_string();
        out.insert(c["id"].as_str()?.to_string(), (field("kind"), field("statement")));
    }
    Some(out)
}

/// The `[pin]` table at `rev`; an absent file holds none, an unreadable one is `None`.
fn pins(rev: &str) -> Option<BTreeMap<String, toml::Value>> {
    let Some(text) = show(rev, PINS) else { return Some(BTreeMap::new()) };
    let value: toml::Table = text.parse().ok()?;
    match value.get("pin") {
        None => Some(BTreeMap::new()),
        Some(pin) => Some(pin.as_table()?.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
    }
}

/// Every `// spec: <id>@<rev>` and `-- spec: <id>@<rev>` tag at HEAD, by clause id.
fn tags() -> Result<BTreeMap<String, Vec<String>>> {
    let out = Command::new("git")
        .args(["-c", "core.quotepath=off", "grep", "-I", "-z", "-E", "^[[:space:]]*(//|--) spec: [a-z0-9.-]+@", "HEAD"])
        .output()?;
    // git grep exits 1 when nothing matches.
    if !out.status.success() && out.status.code() != Some(1) {
        return Err(crate::exited_output("git grep", &out));
    }
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((path, text)) = line.split_once('\0') else { continue };
        let path = path.strip_prefix("HEAD:").unwrap_or(path);
        let Some(id) = text.split_once("spec: ").and_then(|(_, rest)| rest.split_once('@')).map(|(id, _)| id.trim()) else { continue };
        map.entry(id.to_string()).or_default().push(path.to_string());
    }
    Ok(map)
}

/// The parts running a tagged test or theorem in `path`.
fn reach_of_pinned_file(root: &Path, path: &str, packages: &mut Packages) -> Result<Reach> {
    if path.starts_with("formal/") || path.ends_with(".lean") {
        Ok(Some(formal()))
    } else if path.starts_with("apps/") || path.starts_with("packages/") {
        Ok(Some(vec![part("surfaces")]))
    } else if path.ends_with(".rs") {
        match crate::changed_package(root, path) {
            Ok(package) => packages.reach(root, &package),
            Err(_) => Ok(None),
        }
    } else {
        Ok(None)
    }
}

/// The parts running a `spec/pins.toml` entry's artifact: a theorem the formal parts, a test
/// or item the parts of the package its path or crate names.
fn reach_of_pin(root: &Path, pin: &toml::Value, packages: &mut Packages) -> Result<Reach> {
    if pin.get("theorem").is_some() {
        return Ok(Some(formal()));
    }
    let Some(name) = pin.get("test").or_else(|| pin.get("item")).and_then(toml::Value::as_str) else { return Ok(None) };
    let head = name.split("::").next().unwrap_or(name);
    if head.contains('/') {
        return reach_of_pinned_file(root, head, packages);
    }
    packages.reach(root, &head.replace('_', "-"))
}
