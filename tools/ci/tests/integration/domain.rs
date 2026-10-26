//! `topology.package.domain-crate`: the domain crate performs no I/O and every profile
//! links it; `topology.bound-application.domain-neutral`: sector vocabulary in the contract
//! files appears only where a profile, a connector or a consumer supplies it.

use crate::repo_root;
use std::path::Path;
use std::process::Command;

/// Calls reaching a file, a socket, a process, the environment, the terminal or the wall
/// clock: what an adapter does through a port and the domain crate never does itself.
const IO: [&str; 13] = [
    "std::fs",
    "std::process",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "stdin()",
    "stdout()",
    "stderr()",
    "std::env::var",
    "thread::sleep",
    "SystemTime::now",
    "println!",
    "eprintln!",
];

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `contextful-core` holds the pure domain types and the port traits every adapter implements, performs no I/O, and links into every profile.
// spec: topology.package.domain-crate@929f7deb
#[test]
fn the_domain_crate_performs_no_io_and_links_into_every_profile() {
    let mut files = Vec::new();
    sources(&repo_root().join("crates/contextful-core/src"), &mut files);
    assert!(!files.is_empty());
    let mut found = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for call in IO.iter().filter(|c| code.contains(*c)) {
                found.push(format!("{}:{}: {call}", f.display(), n + 1));
            }
        }
    }
    assert!(found.is_empty(), "the domain crate performs I/O:\n{}", found.join("\n"));

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    for profile in ["contextful-control", "contextful-edge", "contextful-full"] {
        let o = Command::new(&cargo)
            .args(["tree", "--locked", "-p", "contextful-cli", "--no-default-features", "--features", profile])
            .args(["-e", "normal", "--prefix", "none", "--format", "{p}"])
            .current_dir(repo_root())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let tree = String::from_utf8_lossy(&o.stdout);
        assert!(tree.lines().any(|l| l.starts_with("contextful-core ")), "`{profile}` links no domain crate");
    }
}

/// Sector vocabulary, lower-cased, a two-word term as its two words. `price` stands alone:
/// `prices` is also the meter's verb, and `ticker` and `diagnosis` name an epoch ticker and
/// a failure's diagnosis in generic contracts.
const SECTOR: [&[&str]; 13] = [
    &["price"],
    &["return", "series"],
    &["sharpe"],
    &["portfolio"],
    &["portfolios"],
    &["trader"],
    &["traders"],
    &["patient"],
    &["patients"],
    &["clinical"],
    &["delisting"],
    &["delisted"],
    &["prediction", "market"],
];

/// The words an item names to show a selected application supplies its domain meaning.
const SUPPLIERS: [&str; 6] = ["profile", "profiles", "connector", "connectors", "consumer", "consumers"];

/// The lower-cased alphanumeric words of `text`, a hyphen splitting words as a space does.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// The sector terms `text` holds, and whether it names a supplier.
fn sector_terms(text: &str) -> (Vec<String>, bool) {
    let w = words(text);
    let terms = SECTOR.iter().filter(|t| w.windows(t.len()).any(|win| win.iter().zip(t.iter()).all(|(a, b)| a == b))).map(|t| t.join(" ")).collect();
    (terms, w.iter().any(|x| SUPPLIERS.contains(&x.as_str())))
}

/// The sector terms `text` holds when it names no supplier.
fn unsupplied(text: &str) -> Vec<String> {
    match sector_terms(text) {
        (_, true) => Vec::new(),
        (terms, false) => terms,
    }
}

/// The engine processes declared data, schemas, policies, time bounds and capabilities. Consumer configuration,
/// connectors, components or explicitly selected evaluation profiles supply domain meanings, business rules and
/// sector-specific metrics.
// spec: topology.bound-application.domain-neutral@d8e61b58
#[test]
fn sector_vocabulary_appears_only_where_a_profile_connector_or_consumer_supplies_it() {
    assert_eq!(unsupplied("A report scores the Sharpe ratio of each return series."), ["return series", "sharpe"]);
    assert!(unsupplied("A financial profile scores the Sharpe ratio of each return series.").is_empty());

    let lock: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo_root().join("spec/spec.lock.json")).unwrap()).unwrap();
    let mut items: Vec<(String, String)> = Vec::new();
    for c in lock["clauses"].as_array().unwrap() {
        let text = format!("{} {}", c["statement"].as_str().unwrap_or_default(), c["why"].as_str().unwrap_or_default());
        items.push((c["id"].as_str().unwrap().to_string(), text));
    }
    for key in ["ledes", "groups"] {
        for (at, text) in lock[key].as_object().unwrap() {
            items.push((format!("{key} {at}"), text.as_str().unwrap_or_default().to_string()));
        }
    }
    assert!(items.len() > 1000, "the lock holds {} items", items.len());
    let supplied = items.iter().filter(|(_, t)| matches!(sector_terms(t), (terms, true) if !terms.is_empty())).count();
    assert!(supplied > 0, "no profile item holds sector vocabulary, so the scan reads nothing");

    let found: Vec<String> = items.iter().flat_map(|(at, t)| unsupplied(t).into_iter().map(move |term| format!("{at}: {term}"))).collect();
    assert!(found.is_empty(), "sector vocabulary outside a profile, connector or consumer:\n{}", found.join("\n"));
}
