//! The read face over a scratch store: registration, the guard, responses, ranked
//! retrieval and the query-time enforcement layer, each through the embedded engine.
#![cfg(feature = "read")]

mod enforce;
mod fulltext;
mod guard;
mod latency;
mod pin;
mod pool;
mod register;
mod respond;
mod retrieve;
mod typed;

use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::{Face, ReadFault, ReadOptions};
use contextful_context::Store;
use contextful_core::grant::{Action, Grant, TablePattern, TenantScope};
use contextful_core::identify::Subject;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::ports::FixedClock;
use contextful_core::read::respond::Response;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_policy::enforce::mask::{Pepper, PEPPER_VAR};
use contextful_policy::enforce::session::{Request, Session};
use contextful_policy::issue::{mint, MintClaims, SeedSigner};
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{verify_inherited_pipe, Admission, AdmittedAuthority};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const AUD: &str = "contextful://acme-research";
pub const PEPPER: &str = "read-face-test-pepper";

pub const MANIFEST: &str = r#"
[[pipeline.tables]]
name = "research/notes"
partition_by = ["tenant"]
agent_description = "Research notes, one partition per tenant."

[pipeline.tables.policy.columns]
author_email = { class = "email", strategy = "hash", combine = "truncate:5" }

[pipeline.tables.policy.limits]
max_rows = 3

[[pipeline.tables]]
name = "research/vendor"

[pipeline.tables.policy.zone]
allow = ["public-cloud:*"]

[[pipeline.tables]]
name = "research/visits"

[pipeline.tables.policy.zone]
allow = ["on-prem:*", "public-cloud:*"]

[pipeline.tables.policy.columns]
case_notes = { class = "phi" }

[[pipeline.tables]]
name = "research/contacts"
partition_by = ["tenant"]

[pipeline.tables.policy.columns]
email = { class = "email", strategy = "hash", combine = "truncate:5" }
handle = { strategy = "hash" }
phone = { strategy = "drop" }
age = { strategy = "drop" }

[pipeline.tables.policy.rows]
predicate = "owner = subject.agent"

[[pipeline.tables.policy.rows.exception]]
when = "subject.agent = 'agent://auditor'"
predicate = "true"

[[pipeline.tables]]
name = "research/quiet"

[pipeline.tables.policy.columns]
secret = { strategy = "drop" }

[[pipeline.tables]]
name = "lab/masks"

[pipeline.tables.policy.columns]
h = { strategy = "hash" }
hc = { strategy = "hash", combine = "truncate:6" }
k = { strategy = "tokenize" }
kc = { strategy = "tokenize", combine = "truncate:4" }
tr = { strategy = "truncate:3" }
b = { strategy = "bucket:5" }
r = { strategy = "range:5" }
d = { strategy = "drop" }
n = { strategy = "bucket:5" }
dn = { strategy = "drop" }

[[pipeline.tables]]
name = "hr/salaries"

[[query_templates]]
id = "notes_for"
sql = "SELECT note_id FROM \"research/notes\" WHERE tenant = ? ORDER BY note_id"
parameters = ["tenant:string"]
"#;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

pub fn pepper() -> Pepper {
    Pepper::resolve(|v| (v == PEPPER_VAR).then(|| PEPPER.to_string()))
}

/// A store holding the fixture's tables, the face over it, and an issuer.
pub struct Reads {
    _dir: tempfile::TempDir,
    pub store: Store,
    pub face: Face,
    signer: SeedSigner,
}

/// Build the model `id` that `models` declares over `face`'s store at `now`.
pub fn build_model(face: &Face, models: &str, id: &str, now: &str) -> contextful_context::build::Built {
    use contextful_context::build::{build, BuildRequest};
    use contextful_core::pipeline::declare::{collect, ManifestFile};
    use contextful_core::pipeline::model::collect_models;
    let files = [ManifestFile { path: "contextful.toml".into(), text: models.to_string() }];
    let spec = collect_models(&files, &collect(&files).unwrap()).unwrap().into_iter().find(|m| m.spec.id == id).unwrap().spec;
    build(face, &BuildRequest { model: &spec, site_id: "site-a", started_at: at(now), completed_at: at(now) }).unwrap()
}

pub fn land_rows(store: &Store, table: &str, run: &str, rows: Value) {
    let decl = TableDecl::parse_pipeline(MANIFEST).unwrap().into_iter().find(|d| d.name == table).unwrap_or_else(|| TableDecl::named(table));
    let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None, taint: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(store, &decl, &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
}

impl Reads {
    pub fn new() -> Reads {
        Reads::with_manifest(MANIFEST)
    }

    pub fn with_manifest(manifest: &str) -> Reads {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "research").unwrap();
        land_rows(
            &store,
            "research/notes",
            "run-0001",
            json!([
                { "note_id": "n1", "tenant": "acme", "title": "Solar battery storage costs fall", "summary": "Cell prices fell again.",
                  "source_url": "https://example.org/solar-battery-storage", "published_at": "2030-01-05", "author_email": "dana@acme.example" },
                { "note_id": "n2", "tenant": "acme", "title": "Battery storage for regional grids", "summary": "Grid operators buy storage.",
                  "source_url": "https://example.org/grids", "published_at": "2029-12-01", "author_email": "dana@acme.example" },
                { "note_id": "n3", "tenant": "acme", "title": "Quarterly hiring plan", "summary": "Two analysts.",
                  "source_url": "https://example.org/solar-battery-storage-hiring", "published_at": "sometime", "author_email": "lee@acme.example" },
                { "note_id": "n4", "tenant": "globex", "title": "Solar battery storage at Globex", "summary": "Globex buys cells.",
                  "source_url": "https://example.org/globex", "published_at": "2030-01-07", "author_email": "kim@globex.example" },
            ]),
        );
        land_rows(
            &store,
            "research/vendor",
            "run-0001",
            json!([{ "item_id": "v1", "title": "Solar battery storage feed", "Unit Price (USD)": 12, "a\"b": "quoted" }]),
        );
        land_rows(
            &store,
            "research/visits",
            "run-0001",
            json!([{ "visit_id": "w1", "ward": "ward-3", "case_notes": "chest pain, stable" }]),
        );
        land_rows(
            &store,
            "research/contacts",
            "run-0001",
            json!([
                { "contact_id": "c1", "tenant": "acme", "owner": "agent://research-loop", "email": "dana@acme.example", "handle": "h1", "phone": "555-0100", "age": 40 },
                { "contact_id": "c2", "tenant": "acme", "owner": "agent://research-loop", "email": "lee@acme.example", "handle": "h1", "phone": "555-0101", "age": 31 },
                { "contact_id": "c3", "tenant": "acme", "owner": "agent://other", "email": "kim@acme.example", "handle": "h3", "phone": "555-0102", "age": 52 },
                { "contact_id": "c4", "tenant": "globex", "owner": "agent://research-loop", "email": "ray@globex.example", "handle": "h4", "phone": "555-0103", "age": 29 },
            ]),
        );
        land_rows(&store, "hr/salaries", "run-0001", json!([{ "employee": "e1", "title": "Battery storage engineer salary" }]));
        land_rows(
            &store,
            "lab/masks",
            "run-0001",
            json!([
                { "id": "m1", "h": "12.7", "hc": "12.7", "k": "12.7", "kc": "12.7", "tr": "Zürich", "b": "12.7", "r": "12.7", "d": "x", "n": 12.7, "dn": 40 },
                { "id": "m2", "h": null, "hc": "a", "k": null, "kc": "a", "tr": "ab", "b": "-3", "r": "-3", "d": null, "n": -3.5, "dn": null },
                { "id": "m3", "h": "", "hc": "", "k": "", "kc": "", "tr": "", "b": "n/a", "r": "n/a", "d": "", "n": 0.0, "dn": 1 },
            ]),
        );
        let face = Face::open(store.clone(), manifest, pepper()).unwrap();
        Reads { _dir: dir, store, face, signer: SeedSigner::generate(SignatureAlgorithm::Ed25519) }
    }

    /// Admit a credential carrying `grants` for `subject`.
    pub fn authority(&self, subject: Subject, grants: Vec<Grant>) -> AdmittedAuthority {
        self.authority_under(subject, grants, MintClaims::default())
    }

    /// Admit a credential carrying `grants` for `subject`, minted under `claims`.
    pub fn authority_under(&self, subject: Subject, grants: Vec<Grant>, claims: MintClaims) -> AdmittedAuthority {
        let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
        let mut req = MintRequest::custody(subject, grants);
        req.lifetime = Lifetime::Requested(900);
        let clock = FixedClock(at("2030-01-01T00:00:00Z"));
        let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer: &self.signer, clock: &clock }).unwrap();
        let token = mint(&plan, &claims, &self.signer).unwrap();
        let keys = StaticPins::parse(&self.signer.public_key_text()).unwrap().keys().unwrap();
        let revocation = RevocationState::default();
        verify_inherited_pipe(&token, &keys, &Admission::new(at("2030-01-01T00:05:00Z"), &revocation).expecting(AUD)).unwrap()
    }

    /// A session for `subject` holding `grants`, its credential signing `zone` where given.
    pub fn session_for(&self, subject: Subject, grants: Vec<Grant>, zone: Option<&str>) -> Session {
        let subject = Subject { zone: zone.map(str::to_string).or(subject.zone), ..subject };
        let authority = self.authority(subject, grants);
        self.face.session(&authority, &Request::default(), Bounds::default()).unwrap()
    }

    pub fn session(&self, tables: &[&str], tenant: Option<(&str, &str)>, zone: Option<&str>) -> Session {
        self.session_for(loop_subject("agent://research-loop"), vec![read(tables, tenant)], zone)
    }

    pub fn query(&self, session: &Session, sql: &str) -> Result<Response, ReadFault> {
        self.face.query(session, sql, ReadOptions::default())
    }
}

pub fn loop_subject(agent: &str) -> Subject {
    Subject { on_behalf_of: Some("user://dana@acme.example".into()), agent: Some(agent.into()), zone: Some("on-prem:hq".into()), ..Subject::default() }
}

pub fn read(tables: &[&str], tenant: Option<(&str, &str)>) -> Grant {
    Grant {
        actions: vec![Action::Read],
        tables: tables.iter().map(|t| TablePattern::parse(t).unwrap()).collect(),
        tenant: tenant.map(|(table, value)| TenantScope { table: table.into(), value: value.into() }),
        aggregate: None,
        templates: None,
        max_rows: None,
    }
}

/// One column of a response, by name.
pub fn column(r: &Response, name: &str) -> Vec<Value> {
    let i = r.columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name} in {:?}", r.columns));
    r.rows.iter().map(|row| row[i].clone()).collect()
}

/// The refusal a read returned, by identifier and message.
pub fn refusal<T: std::fmt::Debug>(r: Result<T, ReadFault>) -> (String, String) {
    match r {
        Err(ReadFault::Refused(refusal)) => (refusal.identifier().to_string(), refusal.to_string()),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

pub fn refused_with<T: std::fmt::Debug>(r: Result<T, ReadFault>, identifier: &str) -> String {
    let (id, message) = refusal(r);
    assert_eq!(id, identifier, "{message}");
    message
}
