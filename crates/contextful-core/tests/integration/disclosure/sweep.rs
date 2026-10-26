//! `disclosure.sweep`: the `[[acl_sweep]]` block, the `--grants` file, and the rows one
//! run commits over a source's current state.

use contextful_core::disclosure::sweep::{
    commit, parse_read, Commit, Events, GrantLine, GrantsRead, Governed, Observation, SourceState, SweepBlock, SweepError, SweepKind,
};
use contextful_core::disclosure::VisibilityError;
use contextful_core::time::Instant;
use std::collections::BTreeSet;

fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

fn governed(ids: &[&str]) -> Vec<Governed> {
    ids.iter().map(|id| Governed { resource_id: id.to_string(), resource_kind: "page".into(), visibility_class: "mirrored".into() }).collect()
}

fn grant(resource: &str, principal: &str) -> GrantLine {
    GrantLine { resource_id: resource.into(), principal: principal.into(), principal_kind: "user".into(), level: "viewer".into() }
}

fn run(kind: SweepKind, id: &str, began: &str, resources: &[&str], grants: Vec<GrantLine>) -> Observation {
    Observation::from_grants_file("wiki", kind, id, at(began), governed(resources), grants)
}

/// The state a committed run leaves for the next one.
fn after(c: &Commit) -> SourceState {
    SourceState { resources: c.resources.clone(), grants: c.grants.clone(), members: c.members.clone(), links: c.links.clone(), freshness: Some(c.freshness.clone()) }
}

fn grant_keys(c: &Commit) -> Vec<(String, String)> {
    c.grants.iter().map(|g| (g.resource_id.clone(), g.principal.clone())).collect()
}

const MANIFEST: &str = r#"
[[acl_sweep]]
source      = "wiki"
credential  = "secret://wiki/permission-reader"
enumerate   = "pages"
full        = { every = "1h" }
incremental = { every = "5m" }
events      = { stream = "wiki.permissions", gap_detectable = true }

[[acl_sweep]]
source      = "drive"
enumerate   = "files"
full        = { every = "1d" }
incremental = { every = "15m" }
"#;

/// An `[[acl_sweep]]` block declares one source's sweep as {{type:contextful_core::disclosure::sweep::RawBlock}}: `enumerate` names the table whose rows are its governed resources, and each cadence is `{ every = <budget> }`.
// spec: disclosure.sweep.sweep-block@726360ee
#[test]
fn an_acl_sweep_block_declares_one_sources_sweep() {
    let blocks = SweepBlock::parse_manifest(MANIFEST).unwrap();
    assert_eq!(blocks.len(), 2);
    let wiki = &blocks[0];
    assert_eq!((wiki.source.as_str(), wiki.enumerate.as_str()), ("wiki", "pages"));
    assert_eq!((wiki.full_every_secs, wiki.incremental_every_secs), (3_600, 300));
    assert_eq!(wiki.credential.as_deref(), Some("secret://wiki/permission-reader"));
    assert!(wiki.events.as_ref().is_some_and(|e| e.stream == "wiki.permissions" && e.gap_detectable));
    let drive = &blocks[1];
    assert_eq!((drive.full_every_secs, drive.incremental_every_secs), (86_400, 900));
    assert!(drive.credential.is_none() && drive.events.is_none());

    assert!(SweepBlock::parse_manifest("[pipeline]\n").unwrap().is_empty());
    // A missing cadence, an unknown key, and a second block for one source refuse.
    for bad in [
        "[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = { every = \"1h\" }\n",
        "[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = { every = \"1h\" }\nincremental = { every = \"5m\" }\naudience = \"all\"\n",
        "[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = { every = \"1h\" }\nincremental = { every = \"5m\" }\n[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = { every = \"2h\" }\nincremental = { every = \"5m\" }\n",
    ] {
        assert!(matches!(SweepBlock::parse_manifest(bad), Err(SweepError::Malformed(_))), "{bad}");
    }
}

/// A sweep cadence's `every` takes the grammar of {{disclosure.bound-staleness.budget-grammar}}.
// spec: disclosure.sweep.cadence-grammar@adf37b7f
#[test]
fn a_cadence_outside_the_budget_grammar_is_refused() {
    for every in ["1h30m", "90", "15w", "0m", ""] {
        let text = format!("[[acl_sweep]]\nsource = \"wiki\"\nenumerate = \"pages\"\nfull = {{ every = \"1h\" }}\nincremental = {{ every = \"{every}\" }}\n");
        match SweepBlock::parse_manifest(&text) {
            Err(SweepError::Visibility(e @ VisibilityError::BudgetMalformed { .. })) => {
                assert_eq!(e.identifier(), "VisibilityBudgetMalformed");
                let shown = e.to_string();
                assert!(shown.contains("acl_sweep `wiki` incremental") && shown.contains(&format!("`{every}`")), "{shown}");
            }
            other => panic!("`{every}` read as {other:?}"),
        }
    }
}

/// A `--grants` file is the run's read of every resource `enumerate` names: one JSON object per line, a {{type:contextful_core::disclosure::sweep::GrantLine}} or `resource_id` with `"unread": true`; an unmarked resource with no grant line holds none.
// spec: disclosure.sweep.grants-file@fd11737c
#[test]
fn a_grants_file_is_the_read_of_every_governed_resource() {
    let text = "{\"resource_id\":\"p1\",\"principal\":\"dana@acme.example\",\"principal_kind\":\"user\",\"level\":\"viewer\"}\n\n{\"resource_id\":\"p1\",\"principal\":\"eng\",\"principal_kind\":\"group\",\"level\":\"editor\"}\n";
    let lines = parse_read(text).unwrap().grants;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1], GrantLine { resource_id: "p1".into(), principal: "eng".into(), principal_kind: "group".into(), level: "editor".into() });
    for (bad, line) in [
        ("{\"resource_id\":\"p1\",\"principal\":\"dana\",\"principal_kind\":\"user\"}", 1),
        ("{\"resource_id\":\"p1\",\"principal\":\"dana\",\"principal_kind\":\"user\",\"level\":\"viewer\",\"audience\":\"all\"}", 1),
        ("\n[1,2]", 2),
        ("{\"resource_id\":\"\",\"principal\":\"dana\",\"principal_kind\":\"user\",\"level\":\"viewer\"}", 1),
    ] {
        match parse_read(bad) {
            Err(SweepError::GrantLine { line: l, .. }) => assert_eq!(l, line, "{bad}"),
            other => panic!("{bad}: {other:?}"),
        }
    }

    // The file reads every governed resource: p2, with no line, holds no grant, and the
    // run covers the source.
    let obs = run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], lines);
    assert_eq!(obs.read, BTreeSet::from(["p1".to_string(), "p2".to_string()]));
    let c = commit(&SourceState::default(), &obs).unwrap();
    assert_eq!(c.resources.len(), 2);
    assert_eq!(c.grants.len(), 2);
    assert!(c.grants.iter().all(|g| g.resource_id == "p1"));
    assert_eq!(c.freshness.watermark_at, Some(at("2030-01-01T00:00:00Z")));

    // An unread marker takes its resource out of the run's read.
    let text = "{\"resource_id\":\"p1\",\"principal\":\"dana\",\"principal_kind\":\"user\",\"level\":\"viewer\"}\n{\"resource_id\":\"p2\",\"unread\":true}\n";
    let read = parse_read(text).unwrap();
    assert_eq!(read.grants.len(), 1);
    assert_eq!(read.unread, BTreeSet::from(["p2".to_string()]));
    let obs = Observation::from_read("wiki", SweepKind::Incremental, "s2", at("2030-01-01T00:05:00Z"), governed(&["p1", "p2"]), read);
    assert_eq!(obs.read, BTreeSet::from(["p1".to_string()]));
    for (bad, line) in [
        ("{\"resource_id\":\"p2\",\"unread\":false}", 1),
        ("{\"resource_id\":\"\",\"unread\":true}", 1),
        ("{\"resource_id\":\"p2\",\"unread\":true,\"level\":\"viewer\"}", 1),
        ("{\"resource_id\":\"p2\",\"unread\":true}\n{\"resource_id\":\"p2\",\"principal\":\"dana\",\"principal_kind\":\"user\",\"level\":\"viewer\"}", 2),
    ] {
        match parse_read(bad) {
            Err(SweepError::GrantLine { line: l, .. }) => assert_eq!(l, line, "{bad}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
}

/// A full run replaces the source's resource and grant rows with those it read, writes a tombstone per grant it no longer observes, and sets `last_full_sweep_at`.
// spec: disclosure.sweep.full-run@8a827141
#[test]
fn a_full_run_replaces_resources_and_grants_and_tombstones_the_withdrawn() {
    let first = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2", "p3"], vec![grant("p1", "dana"), grant("p2", "dana"), grant("p3", "eli")])).unwrap();
    assert!(first.tombstones.is_empty());
    assert_eq!(first.freshness.last_full_sweep_at, Some(at("2030-01-01T00:00:00Z")));
    assert_eq!(first.principals.len(), 2);

    // p3 is gone from the source and dana lost p2.
    let second = commit(&after(&first), &run(SweepKind::Full, "s2", "2030-01-01T01:00:00Z", &["p1", "p2"], vec![grant("p1", "dana")])).unwrap();
    assert_eq!(second.resources.iter().map(|r| r.resource_id.as_str()).collect::<Vec<_>>(), ["p1", "p2"]);
    assert!(second.resources.iter().all(|r| r.sweep_id == "s2" && r.sweep_kind == "full"));
    assert_eq!(grant_keys(&second), [("p1".into(), "dana".into())]);
    assert_eq!(second.grants[0].observed_at, at("2030-01-01T01:00:00Z"));
    let mut tomb: Vec<(&str, &str, Option<&str>)> = second.tombstones.iter().map(|t| (t.scope, t.resource_id.as_str(), t.principal.as_deref())).collect();
    tomb.sort();
    assert_eq!(tomb, [("grant", "p2", Some("dana")), ("grant", "p3", Some("eli")), ("resource", "p3", None)]);
    assert_eq!(second.tombstones.len(), 3);
    assert!(second.tombstones.iter().all(|t| t.revoked_at == at("2030-01-01T01:00:00Z")));
    assert_eq!(second.freshness.last_full_sweep_at, Some(at("2030-01-01T01:00:00Z")));
    assert_eq!(second.principals.len(), 1);
}

/// An incremental run replaces the grants of each resource it read, tombstoning those withdrawn, leaves every other resource's grants standing, and sets `last_incremental_at`.
// spec: disclosure.sweep.incremental-run@52e220c9
#[test]
fn an_incremental_run_replaces_only_the_grants_of_resources_it_read() {
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], vec![grant("p1", "dana"), grant("p2", "dana")])).unwrap();
    // The incremental run reads p2 alone: dana's grant there is withdrawn, eli's arrives.
    let mut obs = run(SweepKind::Incremental, "s2", "2030-01-01T00:05:00Z", &["p1", "p2"], vec![grant("p2", "eli")]);
    obs.read = BTreeSet::from(["p2".to_string()]);
    let inc = commit(&after(&full), &obs).unwrap();
    assert_eq!(grant_keys(&inc), [("p1".into(), "dana".into()), ("p2".into(), "eli".into())]);
    // p1's grant stands as the full run observed it.
    let p1 = inc.grants.iter().find(|g| g.resource_id == "p1").unwrap();
    assert_eq!((p1.sweep_id.as_str(), p1.observed_at), ("s1", at("2030-01-01T00:00:00Z")));
    let p1_row = inc.resources.iter().find(|r| r.resource_id == "p1").unwrap();
    assert_eq!(p1_row.sweep_id, "s1");
    assert_eq!(inc.tombstones.len(), 1);
    assert_eq!((inc.tombstones[0].resource_id.as_str(), inc.tombstones[0].principal.as_deref()), ("p2", Some("dana")));
    assert_eq!(inc.freshness.last_incremental_at, Some(at("2030-01-01T00:05:00Z")));
    assert_eq!(inc.freshness.last_full_sweep_at, Some(at("2030-01-01T00:00:00Z")));
    assert_eq!(inc.freshness.sweep_kind, "incremental");
}

/// A run reading every governed resource sets the source's `watermark_at` to the instant it began reading; lag is now minus `watermark_at`.
// spec: disclosure.sweep.watermark-advance@dca6e630
#[test]
fn a_run_reading_every_governed_resource_moves_the_watermark_to_its_start() {
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], vec![grant("p1", "dana")])).unwrap();
    assert_eq!(full.freshness.watermark_at, Some(at("2030-01-01T00:00:00Z")));
    assert_eq!(full.freshness.lag_secs(at("2030-01-01T00:12:00Z")), Some(720));

    // An incremental run reading p1 alone leaves the watermark where it stood.
    let mut partial = run(SweepKind::Incremental, "s2", "2030-01-01T00:05:00Z", &["p1", "p2"], vec![grant("p1", "dana")]);
    partial.read = BTreeSet::from(["p1".to_string()]);
    let inc = commit(&after(&full), &partial).unwrap();
    assert_eq!(inc.freshness.watermark_at, Some(at("2030-01-01T00:00:00Z")));

    // An incremental run reading both moves it to its own start.
    let covering = commit(&after(&inc), &run(SweepKind::Incremental, "s3", "2030-01-01T00:10:00Z", &["p1", "p2"], vec![grant("p1", "dana")])).unwrap();
    assert_eq!(covering.freshness.watermark_at, Some(at("2030-01-01T00:10:00Z")));
    assert_eq!(covering.freshness.lag_secs(at("2030-01-01T00:10:30Z")), Some(30));

    // A source no run has covered carries no watermark and no lag.
    let mut first = run(SweepKind::Incremental, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], vec![]);
    first.read = BTreeSet::new();
    let uncovered = commit(&SourceState::default(), &first).unwrap();
    assert_eq!((uncovered.freshness.watermark_at, uncovered.freshness.lag_secs(at("2030-01-01T01:00:00Z"))), (None, None));
}

/// A committed run advances its source's epoch by one.
// spec: disclosure.sweep.sweep-epoch@b2175a9d
#[test]
fn each_committed_run_advances_the_source_epoch_by_one() {
    let mut state = SourceState::default();
    for (n, (kind, began)) in [(SweepKind::Full, "2030-01-01T00:00:00Z"), (SweepKind::Incremental, "2030-01-01T00:05:00Z"), (SweepKind::Full, "2030-01-01T01:00:00Z")].into_iter().enumerate() {
        let c = commit(&state, &run(kind, &format!("s{n}"), began, &["p1"], vec![grant("p1", "dana")])).unwrap();
        assert_eq!(c.freshness.epoch, n as u64 + 1);
        assert_eq!(c.freshness.sweep_id, format!("s{n}"));
        state = after(&c);
    }
}

/// A sweep run that failed or skipped any governed resource of its source and advances `watermark_at` raises `VisibilityWatermarkUncovered`, and the watermark stays where it stood.
// spec: disclosure.sweep.coverage-watermark@294031f6
#[test]
fn a_full_run_missing_a_governed_resource_is_refused_and_the_watermark_holds() {
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2", "p3"], vec![grant("p1", "dana")])).unwrap();
    let state = after(&full);
    let mut partial = run(SweepKind::Full, "s2", "2030-01-01T01:00:00Z", &["p1", "p2", "p3"], vec![grant("p1", "dana")]);
    partial.read = BTreeSet::from(["p1".to_string()]);
    match commit(&state, &partial) {
        Err(e @ VisibilityError::WatermarkUncovered { .. }) => {
            assert_eq!(e.identifier(), "VisibilityWatermarkUncovered");
            let shown = e.to_string();
            assert!(shown.starts_with("VisibilityWatermarkUncovered") && shown.contains("wiki") && shown.contains("2 governed") && shown.contains("`p2`"), "{shown}");
        }
        other => panic!("{other:?}"),
    }

    // A grants file marking p3 unread is that same partial read.
    let read = GrantsRead { grants: vec![grant("p1", "dana")], unread: BTreeSet::from(["p3".to_string()]), ..GrantsRead::default() };
    let marked = Observation::from_read("wiki", SweepKind::Full, "s3", at("2030-01-01T02:00:00Z"), governed(&["p1", "p2", "p3"]), read.clone());
    assert!(matches!(commit(&state, &marked), Err(VisibilityError::WatermarkUncovered { missed: 1, .. })));

    // A webhook run always advances the watermark, so an unread resource it names refuses too.
    let mut hook = Observation::from_read("wiki", SweepKind::Webhook, "s4", at("2030-01-01T02:00:00Z"), governed(&["p1", "p2", "p3"]), read);
    hook.events = Some(Events { stream: "wiki.permissions".into(), gap_detectable: true });
    assert!(matches!(commit(&state, &hook), Err(VisibilityError::WatermarkUncovered { missed: 1, .. })));

    // An incremental run moves no watermark, so its partial read commits and the watermark holds.
    let read = GrantsRead { grants: vec![grant("p1", "dana")], unread: BTreeSet::from(["p3".to_string()]), ..GrantsRead::default() };
    let inc = commit(&state, &Observation::from_read("wiki", SweepKind::Incremental, "s5", at("2030-01-01T02:00:00Z"), governed(&["p1", "p2", "p3"]), read)).unwrap();
    assert_eq!(inc.freshness.watermark_at, Some(at("2030-01-01T00:00:00Z")));
}

fn hook(id: &str, began: &str, resources: &[&str], grants: Vec<GrantLine>, gap_detectable: Option<bool>) -> Observation {
    let mut obs = Observation::from_read("wiki", SweepKind::Webhook, id, at(began), governed(resources), GrantsRead { grants, ..GrantsRead::default() });
    obs.events = gap_detectable.map(|gap_detectable| Events { stream: "wiki.permissions".into(), gap_detectable });
    obs
}

/// A webhook run applies one delivery of the source's `events` stream: it replaces the grants of each resource its file names, leaves every other resource standing, and moves `watermark_at` to the instant it began reading.
// spec: disclosure.sweep.webhook-run@35cac212
#[test]
fn a_webhook_run_replaces_the_grants_of_the_resources_it_names() {
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2", "p3"], vec![grant("p1", "dana"), grant("p2", "dana"), grant("p3", "eli")])).unwrap();
    // The delivery names p2 alone: dana's grant there gives way to fay's.
    let c = commit(&after(&full), &hook("s2", "2030-01-01T00:07:00Z", &["p1", "p2", "p3"], vec![grant("p2", "fay")], Some(true))).unwrap();
    assert_eq!(grant_keys(&c), [("p1".into(), "dana".into()), ("p2".into(), "fay".into()), ("p3".into(), "eli".into())]);
    let p2 = c.grants.iter().find(|g| g.resource_id == "p2").unwrap();
    assert_eq!((p2.sweep_kind.as_str(), p2.sweep_id.as_str()), ("webhook", "s2"));
    let p1 = c.grants.iter().find(|g| g.resource_id == "p1").unwrap();
    assert_eq!(p1.sweep_id, "s1");
    assert_eq!(c.resources.len(), 3);
    assert_eq!(c.tombstones.len(), 1);
    assert_eq!((c.tombstones[0].resource_id.as_str(), c.tombstones[0].principal.as_deref()), ("p2", Some("dana")));
    assert_eq!(c.freshness.watermark_at, Some(at("2030-01-01T00:07:00Z")));
    assert_eq!(c.freshness.sweep_kind, "webhook");
    assert_eq!(c.freshness.last_full_sweep_at, Some(at("2030-01-01T00:00:00Z")));
    assert_eq!(c.freshness.last_incremental_at, None);
    assert_eq!(c.freshness.epoch, 2);

    // A grant on a resource the source does not govern has no row.
    assert!(matches!(commit(&after(&c), &hook("s3", "2030-01-01T00:08:00Z", &["p1", "p2", "p3"], vec![grant("p9", "fay")], Some(true))), Err(VisibilityError::OrphanGrant { .. })));
    assert_eq!(SweepKind::parse("webhook"), Some(SweepKind::Webhook));
    assert_eq!(SweepKind::Webhook.as_str(), "webhook");
}

/// A `--grants` line carrying `resource_id` alone names a resource read holding no grant, so a webhook run naming it withdraws every grant it held.
// spec: disclosure.sweep.bare-resource@620c4ef7
#[test]
fn a_bare_resource_line_withdraws_every_grant_a_webhook_names() {
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], vec![grant("p1", "eli"), grant("p2", "dana")])).unwrap();
    // The source revoked dana's only grant on p2: the delivery names p2 bare.
    let read = parse_read("{\"resource_id\":\"p2\"}\n").unwrap();
    assert_eq!((read.grants.len(), read.bare.clone()), (0, BTreeSet::from(["p2".to_string()])));
    let mut obs = Observation::from_read("wiki", SweepKind::Webhook, "s2", at("2030-01-01T00:07:00Z"), governed(&["p1", "p2"]), read);
    obs.events = Some(Events { stream: "wiki.permissions".into(), gap_detectable: true });
    assert_eq!(obs.governed.iter().map(|g| g.resource_id.as_str()).collect::<Vec<_>>(), ["p2"]);
    let c = commit(&after(&full), &obs).unwrap();
    assert_eq!(grant_keys(&c), [("p1".into(), "eli".into())]);
    assert_eq!(c.tombstones.iter().map(|t| (t.scope, t.resource_id.as_str(), t.principal.as_deref())).collect::<Vec<_>>(), [("grant", "p2", Some("dana"))]);
    assert_eq!(c.resources.len(), 2);
    assert_eq!(c.freshness.watermark_at, Some(at("2030-01-01T00:07:00Z")));

    // A bare resource carries neither a grant nor an unread marker.
    for (bad, line) in [
        ("{\"resource_id\":\"\"}", 1),
        ("{\"resource_id\":\"p2\"}\n{\"resource_id\":\"p2\",\"principal\":\"dana\",\"principal_kind\":\"user\",\"level\":\"viewer\"}", 2),
        ("{\"resource_id\":\"p2\",\"principal\":\"dana\",\"principal_kind\":\"user\",\"level\":\"viewer\"}\n{\"resource_id\":\"p2\"}", 2),
        ("{\"resource_id\":\"p2\"}\n{\"resource_id\":\"p2\",\"unread\":true}", 2),
        ("{\"resource_id\":\"p2\",\"unread\":true}\n{\"resource_id\":\"p2\"}", 2),
    ] {
        match parse_read(bad) {
            Err(SweepError::GrantLine { line: l, .. }) => assert_eq!(l, line, "{bad}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
}

/// Advancing `watermark_at` from an event stream the mapping has not declared gap-detectable raises `VisibilityUngappedStream`.
// spec: disclosure.sweep.ungapped-stream@45bd8bc7
#[test]
fn a_webhook_run_from_a_stream_not_declared_gap_detectable_is_refused() {
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], vec![grant("p1", "dana")])).unwrap();
    for gap_detectable in [Some(false), None] {
        match commit(&after(&full), &hook("s2", "2030-01-01T00:07:00Z", &["p1", "p2"], vec![grant("p2", "fay")], gap_detectable)) {
            Err(e @ VisibilityError::UngappedStream { .. }) => {
                assert_eq!(e.identifier(), "VisibilityUngappedStream");
                let shown = e.to_string();
                assert!(shown.starts_with("VisibilityUngappedStream") && shown.contains("wiki"), "{shown}");
                if gap_detectable.is_some() {
                    assert!(shown.contains("wiki.permissions"), "{shown}");
                }
            }
            other => panic!("{gap_detectable:?}: {other:?}"),
        }
    }
}

/// A grant landing with no resource row, or on a resource of the unknown class, raises `VisibilityOrphanGrant` at commit and joins no reachable set.
// spec: disclosure.sweep.orphan-grant@7dcd16ba
#[test]
fn a_grant_with_no_resource_row_or_of_the_unknown_class_is_refused() {
    let orphan = |c: Result<Commit, VisibilityError>, resource: &str| match c {
        Err(e @ VisibilityError::OrphanGrant { .. }) => {
            assert_eq!(e.identifier(), "VisibilityOrphanGrant");
            let shown = e.to_string();
            assert!(shown.starts_with("VisibilityOrphanGrant") && shown.contains(&format!("`{resource}`")), "{shown}");
        }
        other => panic!("{other:?}"),
    };
    // No resource row: p9 is not among the governed resources.
    orphan(commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1"], vec![grant("p1", "dana"), grant("p9", "dana")])), "p9");

    // The unknown class: the enumerating table claims no servable or federated level.
    let mut obs = run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1"], vec![grant("p1", "dana")]);
    obs.governed[0].visibility_class = "unknown".into();
    orphan(commit(&SourceState::default(), &obs), "p1");

    // An incremental grant on a standing resource the run did not read has no row from it.
    let full = commit(&SourceState::default(), &run(SweepKind::Full, "s1", "2030-01-01T00:00:00Z", &["p1", "p2"], vec![])).unwrap();
    let mut inc = run(SweepKind::Incremental, "s2", "2030-01-01T00:05:00Z", &["p1", "p2"], vec![grant("p1", "dana")]);
    inc.read = BTreeSet::from(["p2".to_string()]);
    orphan(commit(&after(&full), &inc), "p1");
}
