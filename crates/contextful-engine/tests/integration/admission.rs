//! `run.own.admission-pin`: a replay resolves its connector artifact from the pin its run
//! was admitted against.

use crate::support::{plan, three_pages, Pages, Rig, Sink};
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::{Artifacts, ConnectorPin};
use contextful_core::run::plan::Plan;
use contextful_core::run::record::RunStatus;
use contextful_core::run::{Failure, FailureTag};
use contextful_engine::RunSpec;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;

/// Admitted artifacts by content hash.
#[derive(Default)]
struct Store(BTreeMap<String, Vec<u8>>);

impl Store {
    fn admit(&mut self, bytes: &[u8]) -> String {
        let hash = sha256_hex(bytes);
        self.0.insert(hash.clone(), bytes.to_vec());
        hash
    }
}

impl Artifacts for Store {
    fn by_hash(&self, hash: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok(self.0.get(hash).cloned())
    }
}

fn spec(p: &Plan, connector: ConnectorPin, run_id: &str) -> RunSpec {
    RunSpec { plan: p.clone(), connector, run_id: run_id.into(), site_id: "site-a".into(), pid: 4242, boot_id: "boot-a".into(), trace_id: None }
}

/// A run pins its connector identity at admission and a replay resolves the artifact from that pin; a connector
/// rebuilt later reaches no in-flight or replayed run.
// spec: run.own.admission-pin@d7885d15
#[test]
fn a_replay_runs_the_artifact_its_pin_names_and_a_rebuild_waits_for_a_fresh_run() {
    let rig = Rig::new();
    let p = plan("kind = \"opaque-token\"", "");
    let mut artifacts = Store::default();
    let built = p.connector_pin(&artifacts.admit(b"connector build 1"));

    // Admitted against build 1, the run dies after the vendor served page 1.
    let (pin, bytes) = rig.engine.admit_connector("feed", "filings", &built, &artifacts).unwrap();
    assert_eq!((&pin, bytes.as_slice()), (&built, &b"connector build 1"[..]));
    let mut source = Pages::new(three_pages());
    source.die_after = Some(1);
    let mut sink = Sink::default();
    let died = std::panic::catch_unwind(AssertUnwindSafe(|| rig.engine.run(&spec(&p, pin.clone(), "run-1"), &mut source, &mut sink)));
    assert!(died.is_err());
    rig.clock.advance(60);

    // The connector is rebuilt; the replay still resolves build 1 by its pin and completes on it.
    let rebuilt = p.connector_pin(&artifacts.admit(b"connector build 2"));
    let (pin, bytes) = rig.engine.admit_connector("feed", "filings", &rebuilt, &artifacts).unwrap();
    assert_eq!((&pin, bytes.as_slice()), (&built, &b"connector build 1"[..]), "the replay resolves the admitted artifact, not the rebuild");
    let replay = rig.engine.run(&spec(&p, pin, "run-2"), &mut source, &mut sink).unwrap();
    assert_eq!(replay.status, RunStatus::Success);
    assert_eq!(replay.connector_hash, built.hash, "the replay ran the pinned build");
    assert!(rig.catalog().owner("feed", "filings").unwrap().is_none(), "the replay cleared its owner");

    // Only a run admitted after the replay closed reaches the rebuild.
    let (pin, bytes) = rig.engine.admit_connector("feed", "filings", &rebuilt, &artifacts).unwrap();
    assert_eq!((&pin, bytes.as_slice()), (&rebuilt, &b"connector build 2"[..]));

    // A pin whose artifact is gone, or whose stored bytes moved, refuses rather than running another build.
    let missing = p.connector_pin(&sha256_hex(b"never admitted"));
    let refused = rig.engine.admit_connector("feed", "filings", &missing, &artifacts).unwrap_err();
    assert_eq!(refused.tag, FailureTag::UnknownConnector);
    artifacts.0.insert(rebuilt.hash.clone(), b"tampered".to_vec());
    let refused = rig.engine.admit_connector("feed", "filings", &rebuilt, &artifacts).unwrap_err();
    assert!(refused.message.contains(&rebuilt.hash), "{}", refused.message);
}
