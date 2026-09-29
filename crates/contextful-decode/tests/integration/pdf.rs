//! PDF page text under the `pdf` feature.
#![cfg(feature = "pdf")]

use contextful_core::run::FailureTag;
use contextful_decode::pdf::pages;

/// A PDF of one Helvetica text line per page; an empty string draws nothing on its page.
/// `trailer` is spliced into the trailer dictionary.
fn build(texts: &[&str], trailer: &str) -> Vec<u8> {
    let n = texts.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    for (i, t) in texts.iter().enumerate() {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + 2 * i));
        let stream = if t.is_empty() { String::new() } else { format!("BT /F1 12 Tf 72 720 Td ({t}) Tj ET") };
        objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R {trailer}>>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
    out
}

/// Each page decodes to its own trimmed text, in page order.
#[test]
fn every_page_decodes_to_its_own_text_in_order() {
    let got = pages(&build(&["Quarterly plan", "", "Hiring targets"], ""), "Team/Plan").unwrap();
    assert_eq!(got, ["Quarterly plan", "", "Hiring targets"], "a blank page inside a textual document keeps its number");
}

/// An encrypted document, and one with no extractable text on any page, is unreadable input under
/// {{run.land.unreadable-input}} and never lands as empty pages.
// spec: connector.source.document-unreadable@3161aca7
#[test]
fn an_encrypted_or_textless_document_refuses_whole_naming_it() {
    let blank = pages(&build(&["", ""], ""), "Team/scan.pdf").unwrap_err();
    assert!(blank.message.starts_with("PipelineUnreadableInput") && blank.message.contains("Team/scan.pdf"), "{blank}");
    assert!(blank.message.contains("no page carries extractable text"), "{blank}");
    let encrypt = "/Encrypt << /Filter /Standard /V 2 /R 3 /Length 128 /P -1 /O <1111111111111111111111111111111111111111111111111111111111111111> /U <2222222222222222222222222222222222222222222222222222222222222222> >> /ID [<00112233445566778899aabbccddeeff> <00112233445566778899aabbccddeeff>] ";
    let locked = pages(&build(&["secret"], encrypt), "Team/locked.pdf").unwrap_err();
    assert!(locked.message.starts_with("PipelineUnreadableInput") && locked.message.contains("Team/locked.pdf"), "{locked}");
    assert!(locked.message.contains("encrypted"), "{locked}");
    let garbage = pages(b"not a pdf at all", "Team/notes.pdf").unwrap_err();
    assert!(garbage.message.starts_with("PipelineUnreadableInput"), "{garbage}");
    for f in [blank, locked, garbage] {
        assert_eq!(f.tag, FailureTag::Permanent);
        assert!(f.deterministic, "a parse refusal spends no retry");
    }
}
