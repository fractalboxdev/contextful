/// A hydrated credential cannot be cloned into another owned buffer.
#[test]
fn hydrated_material_has_no_clone() {
    trybuild::TestCases::new().compile_fail("tests/fixtures/hydrated_clone.rs");
}
