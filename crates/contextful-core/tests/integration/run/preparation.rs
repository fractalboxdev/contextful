//! Recording admission is absent for ordinary destination adapters.
use contextful_core::run::ports::{Commit, Destination, Landed, Marker, Part, Stage};
use contextful_core::run::Failure;

struct Ordinary;
impl Destination for Ordinary {
    fn stage_batch(&mut self, _: Stage) -> Result<Part, Failure> { unreachable!() }
    fn commit(&mut self, _: Commit, _: &dyn Fn() -> Result<(), Failure>) -> Result<Landed, Failure> { unreachable!() }
    fn discard(&mut self, _: &str, _: &str) -> Result<(), Failure> { Ok(()) }
    fn newest_marker(&self, _: &str, _: &str) -> Result<Option<Marker>, Failure> { Ok(None) }
}

#[test]
fn ordinary_adapters_do_not_admit_a_prepared_recording_or_bypass_their_writer() {
    let mut ordinary = Ordinary;
    let plan = contextful_core::run::plan::Plan::compile(b"pipeline='feed'\ntable='messages'\n[connector]\nid='vendor'\nversion='1'\ncommand=['unused']\n").unwrap();
    assert_eq!(ordinary.recording_identity(&plan).unwrap(), None);
    assert!(ordinary.prepare_recorded("messages", Vec::new(), Default::default(), "execution-a").is_err());
}

#[test]
fn a_source_cannot_supply_an_internal_prepared_journal_envelope() {
    let bytes = br#"{"authority":"guessed","prepared":{"tables":{}},"rows":[],"next":null}"#;
    assert!(contextful_core::run::ports::Pull::decode(bytes).is_err());
}

/// A pull carries the inputs its source could not read, and a destination keeping no dead-letter table refuses them
/// rather than dropping them.
#[test]
fn a_pull_carries_its_dead_letters_and_an_ordinary_destination_refuses_them() {
    use contextful_core::run::ports::{dead_letter_table, DeadLetter, Pull};
    let bytes = br#"{"rows":[{"n":1}],"skipped":1,"dead_letters":[{"key":"d/2.jsonl","reason":"PipelineUnreadableInput: line 2","sha256":"ab"}]}"#;
    let pull = Pull::decode(bytes).unwrap();
    let letter = DeadLetter { key: "d/2.jsonl".into(), reason: "PipelineUnreadableInput: line 2".into(), sha256: "ab".into() };
    assert_eq!(pull.dead_letters, std::slice::from_ref(&letter));
    assert_eq!(Pull::decode(&serde_json::to_vec(&pull).unwrap()).unwrap(), pull, "a pull's dead letters round-trip");
    assert!(Pull::decode(br#"{"dead_letters":[{"key":"k","reason":"r","sha256":"s","extra":1}]}"#).is_err(), "a dead letter holds three fields");
    assert_eq!(dead_letter_table("feed_orders"), "feed_orders_dead_letter");
    assert!(Ordinary.stage_dead_letters("feed_orders", "run-1", "site-a", &[letter]).is_err());
}
