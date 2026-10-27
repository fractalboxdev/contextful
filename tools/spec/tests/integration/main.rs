//! The checker's one integration binary. Each suite copies the live corpus into a
//! scratch root, applies one change, and runs the built `contextful-spec` against it.

mod address;
mod anatomy;
mod diagram;
mod grammar;
mod guide;
mod lean;
mod rationale;
mod readable;
mod registry;
mod render;
mod slice;
mod scaffold;
mod size;
mod state;
mod tags;
mod targets;
mod terms;
mod topic;
mod ts_pins;
mod types;
mod upkeep;

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Scratch {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Scratch {
    /// A scratch root holding the corpus and its benchmark ledger with no pins and no
    /// source but the files defining the types its pointers name, so each suite states
    /// every pin it depends on; a ratchet counting source sites starts at zero, and one
    /// counting unpinned clauses past any live count.
    pub fn copy() -> Scratch {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let live = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        copy_dir(&live.join("spec"), &root.join("spec"));
        copy_dir(&live.join(".vale"), &root.join(".vale"));
        let s = Scratch { _dir: dir, root };
        let lock: serde_json::Value = serde_json::from_str(&s.read("spec/spec.lock.json")).unwrap();
        // Each pointed-at type's own item, so no other definition in its file (an error variant,
        // a bound constant) enters the scratch as a source site.
        let mut items: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (path, record) in lock["types"].as_object().into_iter().flatten() {
            let file = record["file"].as_str().unwrap().to_string();
            let text = std::fs::read_to_string(live.join(&file)).unwrap();
            let (kind, name) = (record["kind"].as_str().unwrap(), path.rsplit("::").next().unwrap());
            let keyword = regex::Regex::new(&format!(r"^.*\b{kind}\s+{name}\b")).unwrap();
            let line = text.lines().position(|l| keyword.is_match(l)).unwrap_or_else(|| panic!("no {kind} {name} in {file}")) + 1;
            items.entry(file.clone()).or_default().push(item_at(&text, line));
            let package = Path::new(&file).ancestors().find(|p| live.join(p).join("Cargo.toml").is_file()).unwrap();
            let manifest = package.join("Cargo.toml");
            s.write(&manifest.to_string_lossy(), &std::fs::read_to_string(live.join(&manifest)).unwrap());
        }
        for (file, kept) in items {
            s.write(&file, &(kept.join("\n\n") + "\n"));
        }
        s.write("evals/ledger.toml", &std::fs::read_to_string(live.join("evals/ledger.toml")).unwrap());
        s.write("spec/pins.toml", "[pin]\n\n[floor]\n");
        let terms = s.read("spec/terms/corpus.toml");
        let sourceless: Vec<String> = terms
            .lines()
            .map(|l| match l.split_once(" =").map(|(k, _)| k) {
                Some("registry-committed-sites") => "registry-committed-sites = 0".to_string(),
                Some("clause-unowned") => "clause-unowned = 1000000".to_string(),
                _ => l.to_string(),
            })
            .collect();
        s.write("spec/terms/corpus.toml", &(sourceless.join("\n") + "\n"));
        s
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    pub fn pin(&self, clause: &str, kind: &str, path: &str) {
        let text = self.read("spec/pins.toml");
        let text = text.replacen("[pin]\n", &format!("[pin]\n\"{clause}\" = {{ {kind} = \"{path}\" }}\n"), 1);
        self.write("spec/pins.toml", &text);
    }

    /// Run `lint --check <check> --json` and return every finding code with its message.
    pub fn lint(&self, check: &str) -> Vec<(String, String)> {
        let out = self.cmd(&["lint", "--check", check, "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["code"].as_str().unwrap().to_string(), f["message"].as_str().unwrap().to_string()))
            .collect()
    }

    pub fn cmd(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_contextful-spec"))
            .arg("--root")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap()
    }

    /// Run `scaffold <target> --package <package>` and require success.
    pub fn scaffold(&self, target: &str, package: &str) {
        let out = self.cmd(&["scaffold", target, "--package", package]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// Run `state`, then return `clause`'s pin list under the lock file's `pins`, or `Null`.
    pub fn pin_row(&self, clause: &str) -> serde_json::Value {
        let out = self.cmd(&["state"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let lock: serde_json::Value = serde_json::from_str(&self.read("spec/spec.lock.json")).unwrap();
        lock["pins"][clause].clone()
    }

    /// `clause`'s computed verdict from the lock file's `pins`, once `state` has run.
    pub fn verdict(&self, clause: &str) -> Option<String> {
        self.pin_row(clause)[0]["verdict"].as_str().map(str::to_string)
    }

    /// Replace each `todo!(<message>);` line of `rel` with an assertion over the same
    /// message, so a scaffolded test still names the error or bound it was written for.
    pub fn implement(&self, rel: &str) {
        let text: Vec<String> = self
            .read(rel)
            .lines()
            .map(|l| match l.trim_start().strip_prefix("todo!(").and_then(|m| m.strip_suffix(");")) {
                Some(message) => format!("    assert!(!{message}.is_empty());"),
                None => l.to_string(),
            })
            .collect();
        self.write(rel, &(text.join("\n") + "\n"));
    }

    /// Every clause id of `<contract>.<operation>`, from the lock file.
    pub fn clauses_of(&self, operation: &str) -> Vec<String> {
        let lock: serde_json::Value = serde_json::from_str(&self.read("spec/spec.lock.json")).unwrap();
        lock["clauses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .filter(|id| id.starts_with(&format!("{operation}.")))
            .collect()
    }

    /// How many refusal and limit clauses `<contract>.<operation>` holds, from the lock file.
    pub fn targets(&self, operation: &str) -> usize {
        let lock: serde_json::Value = serde_json::from_str(&self.read("spec/spec.lock.json")).unwrap();
        lock["clauses"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"].as_str().unwrap().starts_with(&format!("{operation}.")))
            .filter(|c| c["kind"] == "refusal" || c["kind"] == "limit")
            .count()
    }

    /// The first clause id of `<contract>.<operation>` in the lock file.
    pub fn clause_of(&self, operation: &str) -> String {
        let lock: serde_json::Value = serde_json::from_str(&self.read("spec/spec.lock.json")).unwrap();
        lock["clauses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .find(|id| id.starts_with(&format!("{operation}.")))
            .unwrap_or_else(|| panic!("no clause under {operation}"))
    }
}

/// The struct or enum declared on 1-based `line` of `text`, with the doc comments and
/// attributes above it, through the brace or semicolon closing it.
fn item_at(text: &str, line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut start = line - 1;
    while start > 0 && {
        let above = lines[start - 1].trim_start();
        above.starts_with("///") || above.starts_with("#[")
    } {
        start -= 1;
    }
    let mut depth = 0i32;
    let mut opened = false;
    for (i, l) in lines.iter().enumerate().skip(line - 1) {
        for ch in l.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if (opened && depth == 0) || (!opened && l.trim_end().ends_with(';')) {
            return lines[start..=i].join("\n");
        }
    }
    panic!("no item closes after line {line}")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

pub fn codes(f: &[(String, String)], code: &str) -> Vec<String> {
    f.iter().filter(|(c, _)| c == code).map(|(_, m)| m.clone()).collect()
}
