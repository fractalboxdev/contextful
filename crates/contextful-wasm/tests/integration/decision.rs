//! The decision module's host: one case text in through linear memory, one decision out.

use contextful_wasm::DecisionModule;

const STUB: &[u8] = include_bytes!("../fixtures/decision-stub.wasm");

#[test]
fn a_decision_module_answers_each_case_through_its_exports() {
    let mut module = DecisionModule::load(STUB).expect("the stub loads");
    for case in [&b"{\"op\":\"covers_name\"}"[..], b"\xff\xfe", b""] {
        let decision = module.decide(case).expect("the stub decides");
        assert_eq!(decision, br#"{"verdict":"covered","error":null,"dimension":null}"#);
    }
}

#[test]
fn a_module_missing_the_decision_exports_does_not_load() {
    // A module exporting nothing: the magic number and version alone.
    let empty = b"\0asm\x01\0\0\0";
    let err = DecisionModule::load(empty).err().expect("an empty module refuses");
    assert!(err.to_string().contains("memory"), "{err}");
}
