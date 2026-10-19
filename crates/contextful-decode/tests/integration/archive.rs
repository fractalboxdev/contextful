//! Zip decompression ahead of a decoder: one member, selected by name where the archive
//! holds several, under a ceiling that refuses rather than truncates.

use contextful_core::run::Fault;
use contextful_decode::unzip;
use std::io::Write;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub(crate) fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, body) in members {
        w.start_file(*name, options).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// An archive decodes its single member, or the member `member` names; several members without `member`, or a named
/// member the archive lacks, is a configuration fault, and a body that is not an archive unreadable input.
// spec: connector.source.zip-member@ba43b67c
#[test]
fn an_archive_lands_its_single_or_named_member_and_refuses_an_ambiguous_or_absent_one() {
    let one = archive(&[("20250101000000.export.CSV", b"1\tUSA\n")]);
    assert_eq!(unzip(&one, None, 1 << 20, "s3://b/one.zip").unwrap(), b"1\tUSA\n");
    let two = archive(&[("a.csv", b"id\n1\n"), ("b.csv", b"id\n2\n")]);
    assert_eq!(unzip(&two, Some("b.csv"), 1 << 20, "s3://b/two.zip").unwrap(), b"id\n2\n");
    for (member, why) in [(None, "2 members"), (Some("c.csv"), "no such member")] {
        let f = unzip(&two, member, 1 << 20, "s3://b/two.zip").unwrap_err();
        assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("s3://b/two.zip") && f.message.contains(why), "{f}");
        assert_eq!(f.fault, Some(Fault::Configuration), "{f}");
    }
    let f = unzip(b"not a zip", None, 1 << 20, "s3://b/plain.zip").unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("s3://b/plain.zip"), "{f}");
    assert_eq!(f.fault, Some(Fault::Input), "{f}");
}

#[test]
fn a_member_refuses_whole_one_byte_past_the_ceiling() {
    let body = vec![b'x'; 4096];
    let zipped = archive(&[("m.csv", &body)]);
    assert_eq!(unzip(&zipped, None, 4096, "at-ceiling").unwrap().len(), 4096);
    let f = unzip(&zipped, None, 4095, "s3://b/big.zip").unwrap_err();
    assert!(f.message.contains("s3://b/big.zip") && f.message.contains("4095"), "{f}");
    assert!(f.deterministic);
    assert_eq!(f.fault, Some(Fault::Input), "a member past the ceiling is the object's fault: {f}");
}

/// A directory entry is no member: an archive holding one directory and one file lands the file.
#[test]
fn a_directory_entry_is_no_member() {
    let mut w = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    w.add_directory("d/", options).unwrap();
    w.start_file("d/m.csv", options).unwrap();
    w.write_all(b"id\n1\n").unwrap();
    let zipped = w.finish().unwrap().into_inner();
    assert_eq!(unzip(&zipped, None, 1 << 20, "s3://b/dir.zip").unwrap(), b"id\n1\n");
}

/// A refusal shows a member name escaped and cut to 256 characters.
#[test]
fn a_refusal_escapes_and_truncates_a_member_name() {
    let two = archive(&[("a.csv", b"id\n1\n"), ("b.csv", b"id\n2\n")]);
    let long = format!("x\n{}", "y".repeat(400));
    let f = unzip(&two, Some(&long), 1 << 20, "s3://b/two.zip").unwrap_err();
    assert!(f.message.contains("no such member") && f.message.contains("x\\n") && !f.message.contains('\n'), "{f}");
    assert!(f.message.contains(&"y".repeat(254)) && !f.message.contains(&"y".repeat(255)), "{f}");
}
