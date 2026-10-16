//! The image source's metadata row and the quotability marker paired with its body.

use crate::support::{request, Never};
use contextful_connectors::image::{check_quotability, fill_body, ImageConfig, ImageSource, Quotability};
use contextful_core::run::ports::Source;
use serde_json::{json, Value};

/// A 2x3 PNG holding a header and no pixel data at all.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x02\x00\x00\x00\x03\x08\x02\x00\x00\x00\x36\x88\x49\xd6";

fn landed() -> Value {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("shot.png"), PNG).unwrap();
    let config = ImageConfig::parse(&json!({"root": "."})).unwrap();
    let mut source = ImageSource::new(config, dir.path());
    let pulled: Value = serde_json::from_slice(&source.pull(&request(None), &Never).unwrap()).unwrap();
    pulled["rows"][0].clone()
}

// spec: connector.source.image-row@904c3cb5
#[test]
fn an_image_lands_its_header_metadata_and_a_null_body_without_decoding_pixels() {
    let row = landed();
    assert_eq!(row["path"], json!("shot.png"));
    assert_eq!(row["sha256"].as_str().unwrap().len(), 64);
    assert_eq!((row["width"].clone(), row["height"].clone()), (json!(2), json!(3)));
    assert_eq!(row["capture_at"], Value::Null);
    assert!(row["modified_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(row["modality"], json!("image"));
    assert_eq!(row["body"], Value::Null);
    assert_eq!(row["quotability"], Value::Null);
    check_quotability(&row).unwrap();
}

// spec: connector.source.quotability-marker@3b21c830
#[test]
fn filling_a_body_records_whether_it_is_verbatim_or_a_reading() {
    let mut row = landed();
    fill_body(&mut row, "INVOICE 42".into(), Quotability::Verbatim);
    assert_eq!((row["body"].clone(), row["quotability"].clone()), (json!("INVOICE 42"), json!("verbatim")));
    fill_body(&mut row, "a receipt on a desk".into(), Quotability::Reading);
    assert_eq!((row["body"].clone(), row["quotability"].clone()), (json!("a receipt on a desk"), json!("reading")));
    check_quotability(&row).unwrap();
}

// spec: connector.source.marker-body-pairing@32df2a3c
#[test]
fn a_body_without_its_marker_or_a_marker_without_its_body_is_refused() {
    for (body, marker) in [(json!("text"), Value::Null), (Value::Null, json!("verbatim")), (json!("text"), json!("guess"))] {
        let mut row = landed();
        row["body"] = body;
        row["quotability"] = marker;
        let refused = check_quotability(&row).unwrap_err();
        assert!(refused.to_string().starts_with("ConnectorQuotabilityMismatch"), "{refused}");
    }
}
