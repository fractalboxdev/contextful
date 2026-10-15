//! `connector.record.provider-attribution`: the run row's map of logical name to answering adapter.

use crate::support::{plan, three_pages, Pages, Rig, Sink};
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::record::{RunRow, RunStatus};
use contextful_core::run::{Failure, FailureTag};
use std::collections::BTreeMap;

/// A source that hydrated two names: one from the environment, one from a lease.
struct Credentialed(Pages);

impl Source for Credentialed {
    fn pull(&mut self, request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        self.0.pull(request, cancel)
    }
    fn credential_providers(&self) -> BTreeMap<String, String> {
        [("speech-vendor".to_string(), "lease".to_string()), ("vendor-token".to_string(), "env".to_string())].into()
    }
}

fn expected() -> BTreeMap<String, String> {
    Credentialed(Pages::new(Vec::new())).credential_providers()
}

/// A run's commit-phase row, and its row on the failure arm, carry `credential_providers`,
/// names only; a row recorded without the field decodes as empty.
// spec: connector.record.provider-attribution@f8f3a804
#[test]
fn the_closing_row_maps_each_hydrated_name_to_its_answering_adapter() {
    let rig = Rig::new();
    let cursor = plan("kind = \"opaque-token\"", "");
    let ok = rig.run(&cursor, "1.0.0", "run-ok", &mut Credentialed(Pages::new(three_pages())), &mut Sink::default()).unwrap();
    assert_eq!(ok.status, RunStatus::Success);
    assert_eq!(ok.credential_providers, expected());

    let mut failing = Pages::new(three_pages());
    failing.fail = vec![Failure::deterministic(FailureTag::Permanent, "vendor refused")];
    let failed = rig.run(&cursor, "1.0.0", "run-failed", &mut Credentialed(failing), &mut Sink::default()).unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(rig.row("run-failed").credential_providers, expected());

    // The record holds names and adapter names alone.
    let text = serde_json::to_string(&rig.row("run-ok")).unwrap();
    assert!(text.contains("\"credential_providers\":{\"speech-vendor\":\"lease\",\"vendor-token\":\"env\"}"), "{text}");

    // A source binding no credential leaves the field off the row, and an older row decodes empty.
    let plain = rig.run(&cursor, "1.0.0", "run-plain", &mut Pages::new(three_pages()), &mut Sink::default()).unwrap();
    assert!(plain.credential_providers.is_empty());
    let mut legacy = serde_json::to_value(&plain).unwrap();
    legacy.as_object_mut().unwrap().remove("credential_providers");
    let decoded: RunRow = serde_json::from_value(legacy).unwrap();
    assert!(decoded.credential_providers.is_empty());
}
