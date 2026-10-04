//! The workspace lock resolves one Arrow and Parquet version across its packages.

use crate::repo_root;
use std::collections::{BTreeMap, BTreeSet};

fn one_arrow_tree(text: &str) -> bool {
    let lock: toml::Value = toml::from_str(text).unwrap();
    let mut versions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for package in lock["package"].as_array().unwrap() {
        let name = package["name"].as_str().unwrap();
        if name == "arrow" || name.starts_with("arrow-") || name == "parquet" {
            versions.entry(name).or_default().insert(package["version"].as_str().unwrap());
        }
    }
    let all: BTreeSet<&str> = versions.values().flat_map(|per_package| per_package.iter().copied()).collect();
    !versions.is_empty() && versions.values().all(|per_package| per_package.len() == 1) && all == BTreeSet::from(["58.4.0"])
}

#[test]
fn mixed_arrow_package_versions_fail_the_tree_check() {
    let mixed = r#"
[[package]]
name = "arrow-array"
version = "58.4.0"

[[package]]
name = "arrow-buffer"
version = "58.5.0"

[[package]]
name = "parquet"
version = "58.4.0"
"#;
    assert!(!one_arrow_tree(mixed), "Arrow package names at different patch versions form two trees");
}

#[test]
fn arrow_and_parquet_resolve_once() {
    let lock = std::fs::read_to_string(repo_root().join("Cargo.lock")).unwrap();
    assert!(one_arrow_tree(&lock), "Arrow and Parquet packages must share the exact 58.4.0 version");
}
