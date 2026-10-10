//! Applied structure and current authority are checked before a store-driven fire.
use super::*;

fn declaration() -> String {
    format!("site_id = \"site\"\n{}\n[[pipeline.tables]]\nname=\"documents\"\nprimary_key=[\"doc_id\"]\ncolumns={{doc_id=\"Utf8\",body=\"Utf8\"}}\n[[pipeline.tables]]\nname=\"scores\"\nwrite_mode=\"append\"\nprimary_key=[\"doc_id\"]\ncolumns={{doc_id=\"Utf8\",score=\"Int64\"}}\n", job("max_in_flight=1\n"))
}

fn write_manifest(p: &Path, declaration: &str) {
    std::fs::write(p.join("contextful.toml"), format!("authoring_posture=\"per_request\"\n{declaration}")).unwrap();
}

fn applied_fire(p: &Path, public: &str, token: &str, version: &str, run: &str, extra: &[(&str, &str)]) -> Output {
    scheduled_host(p, public, token, &["job", "fire", "score-documents", "--project", "research", "--applied", version, "--run-id", run, "--site-id", "site", "--now", "2030-01-01T00:02:00Z"], extra)
}

#[test]
fn an_applied_job_keeps_its_output_contract_after_live_edits() {
    let declaration = declaration();
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    std::fs::write(p.join("scores.jsonl"), "{\"doc_id\":\"prior\",\"score\":9}\n").unwrap();
    ok(&cf(p, &["context", "land", "scores", "--project", "research", "--rows", "scores.jsonl", "--run-id", "prior-scores", "--site-id", "site", "--now", "2030-01-01T00:00:00Z"]));
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    ok(&applied_fire(p, &public, &token, "1", "first", &[]));
    assert_eq!(select(p, "SELECT doc_id FROM scores ORDER BY doc_id"), [["d1"], ["d2"], ["d3"], ["prior"]]);
    write_manifest(p, &declaration.replace("write_mode=\"append\"", "write_mode=\"replace\""));
    ok(&applied_fire(p, &public, &token, "1", "same-applied", &[]));
    // Read under the applied append contract to inspect the actual landing, not the live draft view.
    write_manifest(p, &declaration);
    assert_eq!(select(p, "SELECT doc_id FROM scores ORDER BY doc_id"), [["d1"], ["d2"], ["d3"], ["prior"]]);
    write_manifest(p, &declaration.replace("score=\"Int64\"", "score=\"Utf8\""));
    ok(&applied_fire(p, &public, &token, "1", "same-applied-types", &[]));
    write_manifest(p, &declaration);
    assert_eq!(select(p, "SELECT DISTINCT typeof(score) FROM scores"), [["BIGINT"]], "the applied output keeps its stored numeric type");
}

#[test]
fn standalone_output_edits_create_versions_and_survive_partial_pipeline_apply() {
    let declaration = declaration();
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    std::fs::create_dir(p.join("pipelines")).unwrap();
    let source = "id=\"feed\"\ntables=[\"items\"]\n[source]\nname=\"http\"\nconfig={endpoint=\"https://example.org/items\"}\n";
    std::fs::write(p.join("pipelines/feed.toml"), source).unwrap();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let snapshot = std::fs::read_to_string(p.join(".contextful/control/research/manifest@v1.toml")).unwrap();
    assert!(snapshot.contains("standalone_tables") && snapshot.contains("write_mode = \"append\""), "{snapshot}");
    write_manifest(p, &declaration.replace("write_mode=\"append\"", "write_mode=\"replace\""));
    std::fs::write(p.join("pipelines/feed.toml"), source.replace("example.org/items", "example.org/updated")).unwrap();
    let partial = ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "feed", "--project", "research"], &[]));
    assert!(partial.contains("applied v2"), "{partial}");
    let snapshot = std::fs::read_to_string(p.join(".contextful/control/research/manifest@v2.toml")).unwrap();
    assert!(snapshot.contains("write_mode = \"append\""), "partial apply changes no job output contract: {snapshot}");
    let plan = ok(&scheduled_host(p, &public, &token, &["pipeline", "plan", "--json", "--project", "research"], &[]));
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert!(plan["tables"].as_array().unwrap().iter().any(|t| t["id"] == "table:scores" && t["action"] == "change"), "{plan}");
    let full = ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "--project", "research"], &[]));
    assert!(full.contains("applied v3"), "{full}");
    let unchanged = ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "--project", "research"], &[]));
    assert!(unchanged.contains("unchanged at v3"), "{unchanged}");
}

#[test]
fn applied_input_dedup_survives_a_live_primary_key_edit() {
    let declaration = declaration();
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    std::fs::write(p.join("replacement.jsonl"), "{\"doc_id\":\"d1\",\"body\":\"replacement\"}\n").unwrap();
    ok(&cf(p, &["context", "land", "documents", "--project", "research", "--rows", "replacement.jsonl", "--run-id", "newer-d1", "--site-id", "site", "--now", "2030-01-01T00:00:05Z"]));
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    write_manifest(p, &declaration.replacen("primary_key=[\"doc_id\"]", "primary_key=[]", 1));
    let ledger = p.join("paid.txt");
    ok(&applied_fire(p, &public, &token, "1", "pinned-input", &[("SCORE_LEDGER", ledger.to_str().unwrap())]));
    assert_eq!(std::fs::read_to_string(ledger).unwrap().lines().count(), 3, "the applied keyed input selects one row per document");
    assert_eq!(select(p, "SELECT score FROM scores WHERE doc_id='d1'"), [["11"]]);
}

#[test]
fn live_read_authority_drift_refuses_before_any_recorded_call() {
    let declaration = declaration();
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    let before = p.join("before.txt");
    ok(&applied_fire(p, &public, &token, "1", "before-policy", &[("SCORE_LEDGER", before.to_str().unwrap())]));
    assert_eq!(std::fs::read_to_string(before).unwrap().lines().count(), 3);
    write_manifest(p, &declaration.replace("columns={doc_id=\"Utf8\",body=\"Utf8\"}", "columns={doc_id=\"Utf8\",body=\"Utf8\"}\n[pipeline.tables.policy.columns]\nbody={strategy=\"drop\"}"));
    let after = p.join("after.txt");
    let error = err(&applied_fire(p, &public, &token, "1", "after-policy", &[("SCORE_LEDGER", after.to_str().unwrap())]));
    assert!(error.contains("ApplyValidationRefused") && error.contains("authority"), "{error}");
    assert!(!after.exists(), "current authority is checked before the first call");
}

#[test]
fn an_extensionless_main_declaration_keeps_its_applied_table_contracts() {
    let declaration = declaration();
    let (dir, public, token) = project(&declaration);
    let p = dir.path();
    let custom = p.join("manifest");
    std::fs::copy(p.join("contextful.toml"), &custom).unwrap();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research", "--declaration", "manifest"], &[]));
    let snapshot = std::fs::read_to_string(p.join(".contextful/control/research/manifest@v1.toml")).unwrap();
    assert!(snapshot.contains("name = \"scores\"") && snapshot.contains("write_mode = \"append\""), "{snapshot}");
    std::fs::write(&custom, std::fs::read_to_string(&custom).unwrap().replace("score=\"Int64\"", "score=\"Utf8\"")).unwrap();
    ok(&scheduled_host(p, &public, &token, &["job", "fire", "score-documents", "--project", "research", "--declaration", "manifest", "--applied", "1", "--run-id", "custom-main", "--site-id", "site", "--now", "2030-01-01T00:02:00Z"], &[]));
    assert_eq!(select(p, "SELECT DISTINCT typeof(score) FROM scores"), [["BIGINT"]]);
}

#[test]
fn legacy_snapshot_requires_full_apply_before_job_fire() {
    let (dir, public, token) = project(&declaration());
    let p = dir.path();
    std::fs::create_dir(p.join("pipelines")).unwrap();
    let source = "id=\"feed\"\ntables=[\"items\"]\n[source]\nname=\"http\"\nconfig={endpoint=\"https://example.org/items\"}\n";
    std::fs::write(p.join("pipelines/feed.toml"), source).unwrap();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "import", "--project", "research"], &[]));
    // Reconstruct the legacy snapshot representation, which did not retain these contracts.
    let snapshot_path = p.join(".contextful/control/research/manifest@v1.toml");
    let mut old: toml::Value = toml::from_str(&std::fs::read_to_string(&snapshot_path).unwrap()).unwrap();
    old.as_table_mut().unwrap().remove("standalone_tables");
    std::fs::write(&snapshot_path, toml::to_string(&old).unwrap()).unwrap();
    let ledger = p.join("legacy-paid.txt");
    let env = [("SCORE_LEDGER", ledger.to_str().unwrap())];
    let refused = err(&applied_fire(p, &public, &token, "1", "legacy-refusal", &env));
    assert!(refused.contains("ApplyValidationRefused") && refused.contains("full pipeline apply"), "{refused}");
    assert!(!ledger.exists());
    std::fs::write(p.join("pipelines/feed.toml"), source.replace("example.org/items", "example.org/updated")).unwrap();
    ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "feed", "--project", "research"], &[]));
    let partial = std::fs::read_to_string(p.join(".contextful/control/research/manifest@v2.toml")).unwrap();
    assert!(!partial.contains("standalone_tables"));
    assert!(err(&applied_fire(p, &public, &token, "2", "partial-refusal", &env)).contains("ApplyValidationRefused"));
    assert!(!ledger.exists());
    ok(&scheduled_host(p, &public, &token, &["pipeline", "apply", "--project", "research"], &[]));
    ok(&applied_fire(p, &public, &token, "3", "full-upgrade", &env));
    assert_eq!(std::fs::read_to_string(ledger).unwrap().lines().count(), 3);
}
