//! The workspace lock resolves one Arrow and Parquet version across its packages.

use crate::repo_root;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn arrow_and_parquet_resolve_once() {
    let lock = std::fs::read_to_string(repo_root().join("Cargo.lock")).unwrap();
    let lock: toml::Value = toml::from_str(&lock).unwrap();
    let mut versions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for package in lock["package"].as_array().unwrap() {
        let name = package["name"].as_str().unwrap();
        if name == "arrow" || name.starts_with("arrow-") || name == "parquet" {
            versions.entry(name).or_default().insert(package["version"].as_str().unwrap());
        }
    }
    let split: Vec<_> = versions
        .into_iter()
        .filter(|(_, versions)| versions.len() != 1 || !versions.iter().all(|version| version.starts_with("58.")))
        .collect();
    assert!(split.is_empty(), "Arrow and Parquet must resolve once at version 58: {split:?}");
}
