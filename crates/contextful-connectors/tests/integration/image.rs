//! The image source's metadata row and the quotability marker paired with its body.

use crate::support::{request, resolver, Never, Response, Server};
use contextful_connectors::http::Mediation;
use contextful_connectors::image::{check_quotability, fill_body, ImageConfig, ImageSource, Quotability};
use contextful_core::run::ports::Source;
use serde_json::{json, Value};

/// A 2x3 PNG holding a header and no pixel data at all.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x02\x00\x00\x00\x03\x08\x02\x00\x00\x00\x36\x88\x49\xd6";

/// One read of a directory holding `shot.png`, under `config`.
fn pull(config: Value) -> Result<Value, contextful_core::run::Failure> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("shot.png"), PNG).unwrap();
    let config = ImageConfig::parse(&config).unwrap();
    let mut source = ImageSource::new(config, dir.path(), resolver(vec![]), &Mediation::default()).unwrap();
    Ok(serde_json::from_slice(&source.pull(&request(None), &Never)?).unwrap())
}

fn landed() -> Value {
    pull(json!({"root": "."})).unwrap()["rows"][0].clone()
}

/// A declared body reader receives the image's bytes under its declared, hydrated header and fills the row's body
/// and marker; an answer pairing text with no marker refuses the read.
#[test]
fn a_body_reader_fills_the_row_through_the_mediated_client() {
    let reader = Server::start(|r| match r.header("x-shape") {
        Some("paired") => Response::json(200, r#"{"text": "INVOICE 42", "quotability": "verbatim"}"#),
        _ => Response::json(200, r#"{"text": "INVOICE 42"}"#),
    });
    let body = |shape: &str| json!({"root": ".", "body": {"url": reader.url("/read"), "headers": {"X-Shape": shape}}});
    let pulled = pull(body("paired")).unwrap();
    assert_eq!((pulled["rows"][0]["body"].clone(), pulled["rows"][0]["quotability"].clone()), (json!("INVOICE 42"), json!("verbatim")));
    let seen = reader.received("/read");
    assert!(!seen.is_empty());
    assert_eq!((seen[0].method.as_str(), seen[0].header("content-type"), seen[0].body.as_slice()), ("POST", Some("image/png"), PNG));

    let refused = pull(body("unpaired")).unwrap_err();
    assert!(refused.message.contains("ConnectorQuotabilityMismatch") && refused.message.contains("shot.png"), "{refused}");
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
