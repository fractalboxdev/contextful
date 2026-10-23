//! `assurance.gate.compile-only` and `assurance.gate.compile-groups`: a part's compile-only
//! run repeats the part's own `cargo test` invocations with `--no-run`, and the compile
//! groups name one part per distinct set of those invocations.

use crate::{manifest, stderr, Repo};

/// A `cargo` recording `<CARGO_TARGET_DIR> <args>` per invocation in `log`, answering a
/// `--list` invocation with two ordinary tests; `metadata` and `tree` reach the real cargo.
fn recording(log: &std::path::Path) -> String {
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    format!(
        "case \"$1\" in metadata|tree) exec '{real}' \"$@\" ;; esac\necho \"$CARGO_TARGET_DIR $*\" >> '{}'\n\
         case \" $* \" in *\" --list \"*) printf '%s\\n' 'ordinary_00: test' 'ordinary_01: test' ;; esac\nexit 0\n",
        log.display()
    )
}

/// The stages whose parts run `cargo test`.
const COMPILING: [&str; 4] = ["workspace", "acceptance", "evaluate", "features"];

/// A tree with a featured CLI package, an empty shard map, an acceptance package and
/// gate-tier ledger entries in two package groups.
fn tree() -> Repo {
    let r = Repo::init();
    r.write("crates/contextful-cli/Cargo.toml", &(manifest("contextful-cli", "") + "\n[features]\nextra = []\n"));
    r.write("crates/contextful-cli/src/lib.rs", "pub fn documented() {}\n");
    r.write("crates/contextful-cli/tests/integration/main.rs", "#[test]\nfn ordinary_00() {}\n#[test]\nfn ordinary_01() {}\n");
    r.write("tools/ci/cli-shards.toml", "[test]\n");
    r.write("crates/engine/Cargo.toml", &manifest("contextful-engine", ""));
    r.write("crates/engine/src/lib.rs", "");
    r.write("crates/engine/tests/integration/main.rs", "#[test]\nfn measured() {}\n");
    r.write("crates/acceptance/Cargo.toml", &manifest("contextful-acceptance", ""));
    r.write("crates/acceptance/src/lib.rs", "");
    r.write("crates/acceptance/tests/integration/main.rs", "");
    r.write("spec/spec.lock.json", "{\"clauses\": [{\"id\": \"run.journal.entry-key\"}]}\n");
    let entry = |id: &str, test: &str| {
        format!("[entry.{id}]\nclause = \"run.journal.entry-key\"\nmetric = \"figure.value\"\nkind = \"test\"\ntier = \"gate\"\nmethod = {{ test = \"{test}\" }}\ntarget = {{ op = \"==\", value = 4 }}\n\n")
    };
    r.write("evals/ledger.toml", &(entry("demo-figure", "demo::double::doubles") + &entry("engine-figure", "contextful_engine::measured")));
    r.lock();
    r.commit("a CLI, an engine, an acceptance package and two ledger entries");
    r
}

/// Every part of both tiers in the compiling stages, each fold expanded to its members.
fn compiling_parts(r: &Repo) -> Vec<String> {
    let folds = String::from_utf8_lossy(&r.run_ci(&["stages", "--folds"]).stdout).to_string();
    let mut out: Vec<String> = Vec::new();
    for tier in ["pr", "nightly"] {
        let listed = r.run_ci(&["stages", "--parts", "--tier", tier]);
        assert!(listed.status.success(), "{}", stderr(&listed));
        for part in String::from_utf8_lossy(&listed.stdout).lines() {
            let members: Vec<String> = folds
                .lines()
                .find_map(|l| l.split_once(' ').filter(|(label, _)| *label == part).map(|(_, m)| m.split(' ').map(str::to_string).collect()))
                .unwrap_or_else(|| vec![part.to_string()]);
            out.extend(members.into_iter().filter(|m| COMPILING.contains(&m.split('.').next().unwrap())));
        }
    }
    out
}

/// The cargo invocations `gate --stage <part>` makes with `extra`, one recorded line each.
fn recorded(r: &Repo, part: &str, extra: &[&str]) -> Vec<String> {
    let log = r.root.join("cargo.log");
    let _ = std::fs::remove_file(&log);
    let mut args = vec!["--stage", part];
    args.extend(extra);
    let _ = r.gate_with_cargo(&recording(&log), &args);
    std::fs::read_to_string(&log).unwrap_or_default().lines().map(str::to_string).collect()
}

/// A real run's distinct compiling invocations: each line's arguments before `--`, with
/// `--no-run` appended; cargo refuses `--doc` with `--no-run`.
fn compiled(real: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in real.iter().filter(|l| !l.split(' ').any(|a| a == "--doc")) {
        let prefix = line.split(" -- ").next().unwrap();
        let args = if prefix.split(' ').any(|a| a == "--no-run") { prefix.to_string() } else { format!("{prefix} --no-run") };
        if !out.contains(&args) {
            out.push(args);
        }
    }
    out
}

/// `gate --stage <part> --compile-only` runs each `cargo test` invocation the part runs, in its own target directory with `--no-run` and without its `--` arguments, and nothing else; a CLI shard compiles every shard's invocations.
// spec: assurance.gate.compile-only@89c06e2c
#[test]
fn a_compile_only_part_repeats_the_parts_cargo_invocations_with_no_run() {
    let r = tree();
    let parts = compiling_parts(&r);
    let shards: Vec<&String> = parts.iter().filter(|p| p.strip_prefix("workspace.cli-").is_some_and(|k| k.parse::<usize>().is_ok())).collect();
    assert_eq!(shards.len(), 8, "{parts:?}");
    let mut family: Vec<String> = Vec::new();
    for shard in &shards {
        for line in compiled(&recorded(&r, shard, &[])) {
            if !family.contains(&line) {
                family.push(line);
            }
        }
    }
    assert!(family.iter().any(|l| l.contains("--lib")), "no shard ran the CLI unit tests: {family:?}");
    let mut compiling = 0;
    for part in &parts {
        let expected = if shards.contains(&part) { family.clone() } else { compiled(&recorded(&r, part, &[])) };
        let ran = recorded(&r, part, &["--compile-only"]);
        assert_eq!(ran, expected, "{part}");
        assert!(ran.iter().all(|l| !l.contains("--list")), "{part} listed its tests: {ran:?}");
        compiling += usize::from(!ran.is_empty());
    }
    assert!(compiling >= 8, "too few parts compiled anything: {parts:?}");
    for (part, wanted) in [("workspace.compile", "--workspace"), ("evaluate.foundation", "-p demo"), ("features.binary-all", "--all-features"), ("acceptance", "contextful-acceptance")] {
        let ran = recorded(&r, part, &["--compile-only"]);
        assert!(ran.iter().any(|l| l.contains(wanted)), "{part}: {ran:?}");
    }
}

/// `stages --parts --compile-groups` lists, in run order, the first part of each distinct compile signature — its ordered `--compile-only` invocations and target directory — across both tiers unless `--tier` names one.
// spec: assurance.gate.compile-groups@4d080d76
#[test]
fn the_compile_groups_name_one_part_per_distinct_signature() {
    let r = tree();
    let out = r.run_ci(&["stages", "--parts", "--compile-groups"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let groups: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
    let parts = compiling_parts(&r);
    let signatures: Vec<(String, Vec<String>)> = parts.iter().map(|p| (p.clone(), recorded(&r, p, &["--compile-only"]))).collect();
    let signature = |part: &str| signatures.iter().find(|(p, _)| p == part).map(|(_, s)| s.clone()).unwrap_or_else(|| panic!("`{part}` is no compiling part: {groups:?}"));
    let compiled: Vec<Vec<String>> = groups.iter().map(|g| signature(g)).collect();
    for (i, group) in groups.iter().enumerate() {
        assert!(!compiled[i].is_empty(), "{group} compiles nothing");
        assert_eq!(compiled.iter().position(|s| *s == compiled[i]), Some(i), "{group} repeats an earlier group");
    }
    for (part, sig) in signatures.iter().filter(|(_, s)| !s.is_empty()) {
        assert!(groups.iter().any(|g| signature(g) == *sig), "no group compiles what {part} does");
    }
    assert_eq!(groups.iter().filter(|g| g.starts_with("workspace.cli-") && g.as_str() != "workspace.cli-formal").count(), 1, "{groups:?}");
    assert!(groups.contains(&"workspace.compile".to_string()) && groups.iter().any(|g| g.starts_with("features.")), "{groups:?}");

    let pr = r.run_ci(&["stages", "--parts", "--compile-groups", "--tier", "pr"]);
    let pr: Vec<String> = String::from_utf8_lossy(&pr.stdout).lines().map(str::to_string).collect();
    assert_eq!(pr, groups.iter().filter(|g| !g.starts_with("features.")).cloned().collect::<Vec<_>>());
}
