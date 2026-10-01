//! `assurance.build.dependency-allowlist`: `contextful-ci allowlist` over scratch workspaces
//! whose connector package depends on local stubs named like registry crates.

use crate::{repo_root, stderr, Repo};
use std::process::{Command, Output};

fn allowlist(root: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("allowlist")
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn stub(r: &Repo, name: &str, features: &str) {
    r.write(
        &format!("stubs/{name}/Cargo.toml"),
        &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\n{features}\n"),
    );
    r.write(&format!("stubs/{name}/src/lib.rs"), "");
}

/// A workspace whose connector package declares `list` and depends on `deps`, each a stub.
fn connectors(list: &[&str], deps: &[(&str, &str)]) -> Repo {
    let r = Repo::init();
    // A path dependency under the root joins the workspace unless excluded; these stand in
    // for registry crates.
    r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]\nexclude = [\"stubs\"]\n");
    stub(&r, "regex", "");
    stub(&r, "serde_json", "unbounded_depth = []");
    stub(&r, "fancy-regex", "");
    stub(&r, "left-pad", "");
    let lines: String = deps.iter().map(|(d, extra)| format!("{d} = {{ path = \"../../stubs/{d}\"{extra} }}\n")).collect();
    let quoted: Vec<String> = list.iter().map(|n| format!("\"{n}\"")).collect();
    r.write(
        "crates/contextful-connectors/Cargo.toml",
        &format!(
            "[package]\nname = \"contextful-connectors\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"Apache-2.0\"\n\n[dependencies]\n{lines}\n[package.metadata.contextful]\nauthoring-allowlist = [{}]\n",
            quoted.join(", ")
        ),
    );
    r.write("crates/contextful-connectors/src/lib.rs", "");
    r.lock();
    r.commit("connectors");
    r
}

/// The connector authoring dependency allowlist carries a linear-time regular-expression engine and bounded-depth deserialization, and admits no backtracking regex engine and no unbounded recursive parser.
#[test]
fn the_allowlist_carries_regex_and_serde_json_and_admits_no_backtracking_engine() {
    let ok = connectors(&["regex", "serde_json"], &[("regex", ""), ("serde_json", "")]);
    let o = allowlist(&ok.root);
    assert!(o.status.success(), "{}", stderr(&o));

    let lacking = connectors(&["serde_json"], &[("serde_json", "")]);
    let o = allowlist(&lacking.root);
    assert!(!o.status.success() && stderr(&o).contains("lacks `regex`"), "{}", stderr(&o));

    let backtracking = connectors(&["regex", "serde_json", "fancy-regex"], &[("fancy-regex", "")]);
    let o = allowlist(&backtracking.root);
    assert!(!o.status.success() && stderr(&o).contains("admits `fancy-regex`"), "{}", stderr(&o));

    let unbounded = connectors(&["regex", "serde_json"], &[("serde_json", ", features = [\"unbounded_depth\"]")]);
    let o = allowlist(&unbounded.root);
    assert!(!o.status.success() && stderr(&o).contains("unbounded_depth"), "{}", stderr(&o));

    let off_list = connectors(&["regex", "serde_json"], &[("left-pad", "")]);
    let o = allowlist(&off_list.root);
    assert!(!o.status.success() && stderr(&o).contains("`left-pad`, absent from the connector authoring allowlist"), "{}", stderr(&o));

    let here = allowlist(repo_root());
    assert!(here.status.success(), "{}", stderr(&here));
}
