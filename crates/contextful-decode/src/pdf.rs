//! PDF page text: one string per page, in document order (`connector.source.document-grain`).
//!
//! The reader loads the object graph, opens a document encrypted under the empty user
//! password, and renders every page to plain text. It refuses the whole document rather
//! than landing a subset: an encrypted document and one with no extractable text on any
//! page are unreadable input (`connector.source.document-unreadable`), and a page that
//! fails to render is a partial parse over the whole input (`run.land.partial-parse`).
//! The parser panics on some malformed structures, so a caller runs this behind the
//! decode process boundary (`run.land.parse-boundary`).

use crate::unreadable;
use contextful_core::run::{Failure, FailureTag, RunError};
use pdf_extract::{Document, PlainTextOutput};

/// Every page's text, trimmed, in page order. `input` names the document in a refusal.
pub fn pages(bytes: &[u8], input: &str) -> Result<Vec<String>, Failure> {
    let mut doc = Document::load_mem(bytes).map_err(|e| unreadable(input, "the document structure".into(), format!("not a readable PDF: {e}")))?;
    // A document encrypted only to carry permission flags opens under the empty
    // password; the refusal keys on decryption failing, not on `/Encrypt` being present.
    if doc.is_encrypted() {
        doc.decrypt("").map_err(|e| unreadable(input, "the encryption dictionary".into(), format!("encrypted, and the empty password does not open it: {e}")))?;
    }
    let numbers: Vec<u32> = doc.get_pages().into_keys().collect();
    if numbers.is_empty() {
        // A loader that cannot open an encrypted document reads its page tree as empty.
        if doc.trailer.get(b"Encrypt").is_ok() {
            return Err(unreadable(input, "the encryption dictionary".into(), "encrypted, and the empty password does not open it"));
        }
        return Err(unreadable(input, "the page tree".into(), "the document carries no pages"));
    }
    let mut out = Vec::with_capacity(numbers.len());
    for page in numbers {
        let mut text = String::new();
        {
            let mut sink = PlainTextOutput::new(&mut text);
            pdf_extract::output_doc_page(&doc, &mut sink, page).map_err(|e| {
                Failure::deterministic(
                    FailureTag::Permanent,
                    RunError::PipelinePartialParse(format!("`{input}` stopped at page {page}: {e}; none of its pages land")).to_string(),
                )
            })?;
        }
        out.push(text.trim().to_string());
    }
    if out.iter().all(String::is_empty) {
        return Err(unreadable(input, format!("pages 1..{}", out.len()), "no page carries extractable text"));
    }
    Ok(out)
}
