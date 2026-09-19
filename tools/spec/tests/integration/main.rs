//! The checker's one integration binary. Each suite copies the live corpus into a
//! scratch root, applies one change, and runs the built `contextful-spec` against it.

mod grammar;
mod rationale;
mod slice;
mod scaffold;
mod state;
mod tags;

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Scratch {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Scratch {
    /// A scratch root holding a copy of this repository's `spec/` with no pins, so
    /// each suite states every pin it depends on.
    pub fn copy() -> Scratch {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let live = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
        copy_dir(&live, &root.join("spec"));
        let s = Scratch { _dir: dir, root };
        s.write("spec/pins.toml", "[pin]\n\n[floor]\n");
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
