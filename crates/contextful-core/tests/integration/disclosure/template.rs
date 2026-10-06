//! A disclosure refusal retains its identifier through the read surface.

use contextful_core::disclosure::DisclosureError;
use contextful_core::read::Refusal;

#[test]
fn template_refusal_reaches_the_read_surface() {
    let refusal: Refusal = DisclosureError::TemplateMultiStatement("template `two` holds 2 statements".into()).into();
    assert_eq!(refusal.identifier(), "DisclosureTemplateMultiStatement");
    assert!(refusal.to_string().contains("template `two`"));
}
