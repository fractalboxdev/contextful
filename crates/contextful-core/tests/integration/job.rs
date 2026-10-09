//! `surface.fire`: job blocks against the closed kind union and the store-driven kind's
//! declaration.

use contextful_core::job::{parse_jobs, JobError, JobKind, KINDS, STORE_DRIVEN};

fn registered(name: &str) -> bool {
    name == "score"
}

fn store_driven(extra: &str) -> String {
    format!("[[job]]\nname = \"score-documents\"\nkind = \"store-driven\"\nbody = \"score\"\nstatement = \"SELECT doc_id FROM documents\"\ntables = [\"scores\"]\n{extra}")
}

#[test]
fn a_store_driven_block_validates_with_its_pinned_input_and_concurrency() {
    let jobs = parse_jobs(&store_driven("max_in_flight = 4\nas_of = \"2030-01-01T00:00:00Z\"\n"), &registered).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind_name(), STORE_DRIVEN);
    let JobKind::StoreDriven(d) = &jobs[0].kind else { panic!("{:?}", jobs[0].kind) };
    assert_eq!((d.max_in_flight, d.tables.as_slice()), (4, ["scores".to_string()].as_slice()));
    assert_eq!((d.input.body.as_str(), d.input.as_of.as_deref()), ("score", Some("2030-01-01T00:00:00Z")));
    assert_eq!(KINDS.len(), 7, "the union holds seven kinds");
    for kind in KINDS.iter().filter(|k| **k != STORE_DRIVEN) {
        let jobs = parse_jobs(&format!("[[job]]\nname = \"m\"\nkind = \"{kind}\"\nschedule = \"every 1h\"\n"), &registered).unwrap();
        assert_eq!(jobs[0].kind_name(), *kind);
    }
}

/// A block naming a kind outside the union, an argument vector or a host command raises `JobKindUnknown` at
/// validation.
// spec: surface.fire.job-kind-unknown@b261f015
#[test]
fn a_kind_outside_the_union_or_a_command_raises_job_kind_unknown() {
    for block in [
        "[[job]]\nname = \"x\"\nkind = \"shell\"\n".to_string(),
        store_driven("max_in_flight = 4\ncommand = [\"python\", \"score.py\"]\n"),
        "[[job]]\nname = \"x\"\nkind = \"fold\"\nargv = [\"rm\"]\n".to_string(),
    ] {
        match parse_jobs(&block, &registered) {
            Err(JobError::JobKindUnknown(_)) => {}
            other => panic!("{block}: expected JobKindUnknown, got {other:?}"),
        }
    }
}

/// A `store-driven` block declaring no positive integer `max_in_flight` raises `JobConcurrencyUnset` at
/// validation; no default applies.
// spec: surface.fire.store-driven-concurrency@984ee375
#[test]
fn a_store_driven_block_without_a_positive_max_in_flight_raises_job_concurrency_unset() {
    for extra in ["", "max_in_flight = 0\n", "max_in_flight = -2\n", "max_in_flight = \"4\"\n"] {
        match parse_jobs(&store_driven(extra), &registered) {
            Err(JobError::JobConcurrencyUnset(m)) => assert!(m.contains("score-documents"), "{m}"),
            other => panic!("{extra:?}: expected JobConcurrencyUnset, got {other:?}"),
        }
    }
}

/// A `store-driven` block whose `body` names no body the embedding binary registers raises `JobBodyUnregistered`
/// at validation.
// spec: surface.fire.store-driven-body@c7d2b211
#[test]
fn a_store_driven_block_naming_an_unregistered_body_raises_job_body_unregistered() {
    let block = store_driven("max_in_flight = 2\n").replace("body = \"score\"", "body = \"rank\"");
    match parse_jobs(&block, &registered) {
        Err(JobError::JobBodyUnregistered(m)) => assert!(m.contains("`rank`"), "{m}"),
        other => panic!("expected JobBodyUnregistered, got {other:?}"),
    }
}

fn targets(manifest: &str) -> contextful_core::job::Targets {
    use contextful_core::pipeline::declare::{collect, ManifestFile};
    let specs: Vec<_> = collect(&[ManifestFile { path: "contextful.toml".into(), text: manifest.into() }]).unwrap().into_iter().map(|d| d.spec).collect();
    contextful_core::job::Targets::new(&specs, &parse_jobs(manifest, &registered).unwrap(), &[contextful_core::pipeline::model::ModelSpec { id: "daily".into(), sql: "SELECT 1".into(), sql_file: None, disclosure_opt_out: None, disclosure: None, materialized: None, unique_key: None, publish: None, contract: None, freshness: None, tests: vec![] }])
}

const PIPELINES: &str = "[[pipeline]]\nid = \"meta_ads\"\ntables = [\"insights\", { name = \"spend\", columns = { day = \"timestamp\", usd = \"double\" } }]\n\
                         [pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"https://api.vendor.example/v1\" }\n\n";

/// A `fold` target naming no produced table, or a `build` target naming no declared SQL model,
/// raises `JobTargetUnbound`; a target the manifest produces binds.
#[test]
fn a_job_target_binds_to_a_produced_table() {
    use contextful_core::job::bind_targets;
    let bound = format!(
        "{PIPELINES}[[job]]\nname = \"f\"\nkind = \"fold\"\ntarget = \"meta_ads_insights\"\n[[job]]\nname = \"b\"\nkind = \"build\"\ntarget = \"daily\"\n\
         [[job]]\nname = \"s\"\nkind = \"fold\"\ntarget = \"scores\"\n{}",
        store_driven("max_in_flight = 1\n")
    );
    bind_targets(&parse_jobs(&bound, &registered).unwrap(), &targets(&bound)).unwrap();
    for (job, target) in [("fold", "meta_ads_clicks"), ("build", "meta_ads_insights"), ("build", "meta_ads_spend"), ("build", "warehouse")] {
        let m = format!("{PIPELINES}[[job]]\nname = \"j\"\nkind = \"{job}\"\ntarget = \"{target}\"\n");
        match bind_targets(&parse_jobs(&m, &registered).unwrap(), &targets(&m)) {
            Err(JobError::JobTargetUnbound(msg)) => assert!(msg.contains(target) && msg.contains(job), "{msg}"),
            other => panic!("{job} {target}: expected JobTargetUnbound, got {other:?}"),
        }
    }
}

/// A job target naming a destination table in a spelling the fold does not produce raises
/// `PipelineUnboundTableName`, printing the expected spelling.
// spec: run.declare.unbound-table-name@c61fdcef
#[test]
fn a_misspelled_target_names_the_produced_spelling() {
    use contextful_core::job::bind_targets;
    let m = format!("{PIPELINES}[[job]]\nname = \"f\"\nkind = \"fold\"\ntarget = \"Meta-Ads.Insights\"\n");
    match bind_targets(&parse_jobs(&m, &registered).unwrap(), &targets(&m)) {
        Err(JobError::PipelineUnboundTableName(msg)) => assert!(msg.contains("`meta_ads_insights`"), "{msg}"),
        other => panic!("expected PipelineUnboundTableName, got {other:?}"),
    }
}

#[test]
fn build_dependency_is_explicit_and_cannot_have_a_second_schedule() {
    let declaration = "[[job]]\nname='inventory'\nkind='build'\ntarget='inventory'\nafter='source'\n";
    assert!(parse_jobs(declaration, &registered).is_ok());
    assert!(parse_jobs(&format!("{declaration}schedule='every 1m'\n"), &registered).is_err());
    assert!(parse_jobs(&declaration.replace("kind='build'", "kind='fold'"), &registered).is_err());
}

#[test]
fn dependent_build_joins_the_pipeline_unit_after_all_landing_steps() {
    let files = [contextful_core::pipeline::declare::ManifestFile { path: "test.toml".into(), text: "[[pipeline]]\nid='source'\ntables=['items']\n[pipeline.source]\nname='http'\nconfig={endpoint='https://example.com'}\n[[pipeline]]\nid='tail'\nafter='source'\ntables=['items']\n[pipeline.source]\nname='http'\nconfig={endpoint='https://example.com'}\n".into() }];
    let specs: Vec<_> = contextful_core::pipeline::declare::collect(&files).unwrap().into_iter().map(|p| p.spec).collect();
    let declaration = "[[job]]\nname='inventory'\nkind='build'\ntarget='inventory'\nafter='source'\n";
    let jobs = parse_jobs(declaration, &registered).unwrap();
    let runs = contextful_core::job::dependent_jobs(&specs, &jobs).unwrap();
    assert_eq!(runs.steps["source"], ["tail", "job:inventory"]);
    assert_eq!(runs.head_of["job:inventory"], "source");
    let jobs = parse_jobs(&declaration.replace("after='source'", "after='job:inventory'"), &registered).unwrap();
    assert!(contextful_core::job::dependent_jobs(&specs, &jobs).is_err());
}
