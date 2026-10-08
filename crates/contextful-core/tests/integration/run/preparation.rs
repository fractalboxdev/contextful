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
