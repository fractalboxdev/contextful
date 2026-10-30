//! The `image` source's body fill through the built binary, against a loopback reader that
//! answers each posted image with its text and quotability marker.
#![cfg(feature = "data-plane")]

use crate::pipeline::{cf, ok, project, stderr};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

/// A 2x3 PNG holding a header and no pixel data.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x02\x00\x00\x00\x03\x08\x02\x00\x00\x00\x36\x88\x49\xd6";
/// A 2x3 JPEG holding a frame header and no scan data.
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x04\x00\x00\xff\xc0\x00\x11\x08\x00\x03\x00\x02\x03\x01\x11\x00\x02\x11\x00\x03\x11\x00\xff\xd9";

/// One request the reader received.
#[derive(Debug, Clone)]
struct Posted {
    method: String,
    content_type: String,
    body: Vec<u8>,
}

struct Reader {
    port: u16,
    posted: Arc<Mutex<Vec<Posted>>>,
}

impl Reader {
    /// A reader answering each posted image with `answer(bytes)`.
    fn start(answer: impl Fn(&[u8]) -> String + Send + 'static) -> Reader {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let posted: Arc<Mutex<Vec<Posted>>> = Arc::default();
        let log = posted.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let method = line.split_whitespace().next().unwrap_or_default().to_string();
                let (mut length, mut content_type) = (0, String::new());
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        if k.trim().eq_ignore_ascii_case("content-type") {
                            content_type = v.trim().to_string();
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let text = answer(&body);
                log.lock().unwrap().push(Posted { method, content_type, body });
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
            }
        });
        Reader { port, posted }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/read", self.port)
    }

    fn posted(&self) -> Vec<Posted> {
        self.posted.lock().unwrap().clone()
    }
}

fn manifest(reader: &Reader) -> String {
    format!(
        "[[pipeline]]\nid = \"photos\"\ntables = [{{ name = \"images\", primary_key = [\"path\"] }}]\n[pipeline.source]\nname = \"image\"\n[pipeline.source.config]\nroot = \"photos\"\n[pipeline.source.config.body]\nurl = \"{}\"\ntext = \"/reading/text\"\nquotability = \"/reading/kind\"\n",
        reader.url()
    )
}

fn photos(dir: &Path) {
    std::fs::create_dir_all(dir.join("photos")).unwrap();
    std::fs::write(dir.join("photos/receipt.png"), PNG).unwrap();
    std::fs::write(dir.join("photos/street.jpg"), JPEG).unwrap();
}

fn fire(dir: &Path) -> Output {
    cf(dir, &["pipeline", "run", "photos", "--project", "research", "--run-id", "run-1", "--site-id", "site-a", "--now", "2030-01-01T00:00:00Z"])
}

/// Each image's bytes go to the declared reader through the mediated client; the row lands the reader's text and
/// its quotability marker: `verbatim` for text extracted from the image, `reading` for a model's description of a
/// photograph holding no text to quote. The rest of the row is the header metadata of an unfilled image.
// spec: connector.source.image-body@b4e9c02c
// spec: connector.source.quotability-marker@3b21c830
// spec: connector.source.image-row@904c3cb5
#[test]
fn an_image_run_lands_each_body_beside_its_quotability_marker() {
    let reader = Reader::start(|bytes| {
        if bytes.starts_with(b"\x89PNG") {
            r#"{"reading": {"text": "INVOICE 42", "kind": "verbatim"}}"#.to_string()
        } else {
            r#"{"reading": {"text": "a street at dusk", "kind": "reading"}}"#.to_string()
        }
    });
    let dir = project(&manifest(&reader));
    photos(dir.path());
    ok(&cf(dir.path(), &["pipeline", "validate"]));
    assert!(reader.posted().is_empty(), "validation posts no image");

    let out = ok(&fire(dir.path()));
    assert!(out.contains("2 rows"), "{out}");
    let posted = reader.posted();
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert!(posted.iter().all(|p| p.method == "POST"), "{posted:?}");
    assert_eq!((posted[0].content_type.as_str(), posted[0].body.as_slice()), ("image/png", PNG));
    assert_eq!((posted[1].content_type.as_str(), posted[1].body.as_slice()), ("image/jpeg", JPEG));

    let sql = "SELECT path, body, quotability, width, height, modality FROM photos_images ORDER BY path";
    let out: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["query", "--json", "--project", "research", sql]))).unwrap();
    assert_eq!(
        out["rows"],
        serde_json::json!([
            ["receipt.png", "INVOICE 42", "verbatim", "2", "3", "image"],
            ["street.jpg", "a street at dusk", "reading", "2", "3", "image"],
        ])
    );
}

/// A reader answering text without a marker, a marker without text, or a marker of neither spelling fails the
/// fire with `ConnectorQuotabilityMismatch` naming the image, and lands no row.
// spec: connector.source.marker-body-pairing@32df2a3c
#[test]
fn a_body_the_reader_leaves_unmarked_fails_the_fire_naming_the_image() {
    let answers = [
        r#"{"reading": {"text": "INVOICE 42"}}"#,
        r#"{"reading": {"kind": "verbatim"}}"#,
        r#"{"reading": {"text": "INVOICE 42", "kind": "guess"}}"#,
    ];
    for answer in answers {
        let reader = Reader::start(move |_| answer.to_string());
        let dir = project(&manifest(&reader));
        photos(dir.path());
        let out = fire(dir.path());
        let err = stderr(&out);
        assert!(!out.status.success(), "{answer}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(err.contains("ConnectorQuotabilityMismatch") && err.contains("receipt.png"), "{answer}: {err}");
        assert!(!reader.posted().is_empty(), "{answer}: the image reached the reader");
    }
}

/// An answer holding neither text nor marker leaves the row's body and marker null.
#[test]
fn a_reader_finding_no_text_leaves_the_body_and_marker_null() {
    let reader = Reader::start(|_| "{}".to_string());
    let dir = project(&manifest(&reader));
    photos(dir.path());
    ok(&fire(dir.path()));
    let sql = "SELECT path, body, quotability FROM photos_images ORDER BY path";
    let out: serde_json::Value = serde_json::from_str(&ok(&cf(dir.path(), &["query", "--json", "--project", "research", sql]))).unwrap();
    assert_eq!(out["rows"], serde_json::json!([["receipt.png", null, null], ["street.jpg", null, null]]));
}

/// A `body` block naming an unknown key or no `url` fails validation naming it, before any request.
#[test]
fn a_malformed_body_declaration_fails_validation_naming_its_key() {
    let reader = Reader::start(|_| "{}".to_string());
    let good = manifest(&reader);
    let cases = [
        (good.replace("text = ", "texts = "), "texts"),
        (good.replace(&format!("url = \"{}\"\n", reader.url()), ""), "url"),
        (good.replace("text = \"/reading/text\"", "text = \"reading.text\""), "body.text"),
    ];
    for (declaration, key) in cases {
        assert_ne!(declaration, good, "the case for `{key}` changes the declaration");
        let out = cf(project(&declaration).path(), &["pipeline", "validate"]);
        assert!(!out.status.success(), "{key}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(stderr(&out).contains(key), "{key}: {}", stderr(&out));
    }
    assert!(reader.posted().is_empty(), "validation posts no image");
}
