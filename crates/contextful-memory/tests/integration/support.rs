//! A scratch store with a notes table and a claims table, credentials, and a scripted
//! inference endpoint.

use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::grant::{Action, Grant, TablePattern};
use contextful_core::identify::Subject;
use contextful_core::issue::{IssuancePolicy, Lifetime, MintContext, MintRequest, NodeRole, SignatureAlgorithm};
use contextful_core::memory::synthesize::{Inference, Message};
use contextful_core::ports::FixedClock;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_policy::enforce::mask::Pepper;
use contextful_policy::issue::{mint, MintClaims, SeedSigner};
use contextful_policy::keyset::{KeySource, StaticPins};
use contextful_policy::revoke::RevocationState;
use contextful_policy::verify::{verify_local_bearer, Admission, AdmittedAuthority};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

pub const AUD: &str = "contextful://acme-research";

pub const MANIFEST: &str = r#"
[[pipeline.tables]]
name = "research/notes"

[[table]]
name = "memory/facts"
shape = "memory_facts"
columns = ["claim_id", "subject", "predicate", "object", "scope", "tier", "confidence", "valid_from", "valid_to", "evidence", "superseded_by", "grant_id", "agent"]

[[table]]
name = "memory/entities"
shape = "memory_entities"
columns = ["entity_id", "kind", "name", "aliases"]

[[table]]
name = "memory/episodes"
shape = "memory_episodes"
columns = ["episode_id", "source", "observed_at", "text", "evidence"]
"#;

pub fn at(s: &str) -> Instant {
    Instant::parse(s).unwrap()
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub face: Face,
    /// The machine-local directory pass cursors live in.
    pub state: std::path::PathBuf,
    signer: SeedSigner,
}

pub fn land_rows(face: &Face, table: &str, run: &str, rows: Value) {
    let rows = rows.as_array().unwrap().iter().map(|r| r.as_object().unwrap().clone()).collect();
    let ctx = RunContext {
        node: NodeId::parse("ingest-a").unwrap(),
        injection: Injection { run_id: run.into(), site_id: "site-a".into(), batch_seq: Some(0), authored_by: None },
        committed_at: at("2030-01-10T00:00:00Z"),
    };
    land(face.store(), &TableDecl::named(table), &Batch { rows, types: HashMap::new() }, &ctx).unwrap();
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), "research").unwrap();
        let face = Face::open(store, MANIFEST, Pepper::resolve(|_| None)).unwrap();
        let state = dir.path().join("state");
        Fixture { dir, face, state, signer: SeedSigner::generate(SignatureAlgorithm::Ed25519) }
    }

    pub fn authority(&self, agent: &str, actions: &[Action], tables: &[&str]) -> AdmittedAuthority {
        let policy = IssuancePolicy::parse(&format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n")).unwrap();
        let subject = Subject {
            on_behalf_of: Some("user://dana@acme.example".into()),
            agent: Some(agent.into()),
            zone: Some("on-prem:hq".into()),
            ..Subject::default()
        };
        let grant = Grant {
            actions: actions.to_vec(),
            tables: tables.iter().map(|t| TablePattern::parse(t).unwrap()).collect(),
            tenant: None,
            aggregate: None,
            templates: None,
            max_rows: None,
        };
        let mut req = MintRequest::custody(subject, vec![grant]);
        req.lifetime = Lifetime::Requested(900);
        let clock = FixedClock(at("2030-01-01T00:00:00Z"));
        let plan = policy.check(&req, &MintContext { node: NodeRole::Primary, signer: &self.signer, clock: &clock }).unwrap();
        let token = mint(&plan, &MintClaims::default(), &self.signer).unwrap();
        let keys = StaticPins::parse(&self.signer.public_key_text()).unwrap().keys().unwrap();
        let revocation = RevocationState::default();
        verify_local_bearer(&token, &keys, &Admission::new(at("2030-01-01T00:05:00Z"), &revocation).expecting(AUD)).unwrap()
    }

    pub fn writer(&self) -> AdmittedAuthority {
        self.authority("agent://synthesizer", &[Action::Read, Action::Write], &["research/*", "memory/*"])
    }
}

/// An endpoint answering each call with the next scripted content, recording what it was sent.
pub struct Scripted {
    pub answers: Mutex<Vec<String>>,
    pub sent: Mutex<Vec<Vec<Message>>>,
}

impl Scripted {
    pub fn new(answers: &[String]) -> Scripted {
        Scripted { answers: Mutex::new(answers.iter().rev().cloned().collect()), sent: Mutex::new(Vec::new()) }
    }

    pub fn calls(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl Inference for Scripted {
    fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.sent.lock().unwrap().push(messages.to_vec());
        self.answers.lock().unwrap().pop().ok_or_else(|| "no scripted answer left".to_string())
    }
}

pub fn claims(subject: &str, predicate: &str, object: &str, run: &str) -> String {
    json!({ "claims": [{ "subject": subject, "predicate": predicate, "object": object, "confidence": 0.9,
        "evidence": [{ "table": "research/notes", "run": run, "seq": 0 }] }] })
    .to_string()
}
