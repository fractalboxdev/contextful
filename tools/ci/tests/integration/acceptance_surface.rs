//! `assurance.test.acceptance-surface`.

use crate::{manifest, stderr, Repo};

fn with_acceptance(deps: &str) -> Repo {
    let r = Repo::init();
    r.write("crates/acceptance/Cargo.toml", &manifest("contextful-acceptance", deps));
    r.write("crates/acceptance/src/lib.rs", "");
    r.write("crates/acceptance/tests/integration/main.rs", "#[test]\nfn m00_reach() {}\n");
    r.commit("acceptance");
    r
}

#[test]
fn an_acceptance_package_linking_a_workspace_package_is_refused() {
    let r = with_acceptance("demo = { path = \"../demo\" }\n");
    let o = r.gate(&["--stage", "acceptance"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("AcceptanceLinksEngine"), "{}", stderr(&o));
}

#[test]
fn an_acceptance_package_linking_nothing_runs_its_suite() {
    let r = with_acceptance("");
    let o = r.gate(&["--stage", "acceptance"]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn acceptance_limits_simultaneous_scenarios() {
    let r = with_acceptance("");
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let o = r.gate_with_cargo(
        &format!("case \"$1\" in metadata|tree) exec '{real}' \"$@\" ;; esac\necho \"$*\" >&2\n"),
        &["--stage", "acceptance"],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("test -p contextful-acceptance -- --test-threads=4"), "{}", stderr(&o));
}
