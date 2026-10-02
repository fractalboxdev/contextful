//! `connector.source` over the `file` source, against folders built in a temporary directory.

use crate::support::Never;
use contextful_connectors::boundary::{Boundary, PageDecoder};
use contextful_connectors::file::{FileConfig, FileSource, CHUNK_BYTES, MAX_FILE_BYTES};
use contextful_connectors::http::ConfigError;
use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{PullRequest, Row, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

/// A PDF stand-in: the body after its `%PDF` line, one page per form feed.
struct Pages;

impl PageDecoder for Pages {
    fn pages(&self, body: &[u8], input: &str) -> Result<Vec<String>, Failure> {
        let text = String::from_utf8_lossy(body);
        let rest = text.strip_prefix("%PDF-stub\n").ok_or_else(|| Failure::deterministic(FailureTag::Permanent, RunError::PipelineUnreadableInput(format!("`{input}` at byte 0: no stub header")).to_string()))?;
        Ok(rest.split('\u{c}').map(|p| p.trim().to_string()).collect())
    }
}

fn write(root: &Path, path: &str, body: impl AsRef<[u8]>) {
    let p = root.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn source(dir: &Path, config: Value) -> FileSource {
    let config = FileConfig::parse(&config).unwrap();
    FileSource::new(config, dir, Some(Arc::new(Pages)))
}

fn read(s: &FileSource, position: Option<&Value>) -> Result<(Vec<Row>, Value, Value), Failure> {
    let r = s.read(position, &Never)?;
    Ok((r.rows, r.position, json!(r.declined)))
}

fn keys(rows: &[Row]) -> Vec<(String, u64, bool)> {
    rows.iter().map(|r| (r["slug"].as_str().unwrap().to_string(), r["ordinal"].as_u64().unwrap(), r["removed"].as_bool().unwrap())).collect()
}

fn row<'a>(rows: &'a [Row], slug: &str, ordinal: u64) -> &'a Row {
    rows.iter().find(|r| r["slug"] == json!(slug) && r["ordinal"] == json!(ordinal)).unwrap_or_else(|| panic!("no row {slug}#{ordinal} in {rows:?}"))
}

fn long_note() -> String {
    let filler = "word ".repeat(CHUNK_BYTES / 5);
    format!("Intro before any heading.\n\n# Guide\n\n{filler}\n\n## Setup\n\nInstall it.\n\n```sh\n# not a heading\n```\n\n## Setup\n\nAgain.\n")
}

/// A directory walk reaches the configured root and nothing outside it, skips dot-entries, and traverses in sorted
/// order. A decode-selecting key narrows nothing.
// spec: connector.source.walk-boundary@ce5f1f88
#[test]
fn the_walk_stays_under_the_root_skips_dot_entries_and_visits_in_sorted_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("notes");
    write(dir.path(), "outside.md", "# Outside");
    write(&root, "b.md", "B");
    write(&root, "a/z.txt", "Z");
    write(&root, "a/b/c.md", "C");
    write(&root, ".obsidian/workspace.md", "hidden");
    write(&root, "a/.draft.md", "hidden");
    let (rows, _, _) = read(&source(dir.path(), json!({"root": "notes"})), None).unwrap();
    let paths: Vec<&str> = rows.iter().map(|r| r["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["a/b/c.md", "a/z.txt", "b.md"]);
    // The source reads no format key: what lands follows the extension alone.
    let refused = FileConfig::parse(&json!({"root": "notes", "format": "csv"})).unwrap_err();
    assert!(matches!(refused, ConfigError::Run(RunError::PipelineUnknownConfigKey(_))), "{refused}");
}

/// The `file` walk follows no symbolic link, to a file or a directory.
// spec: connector.source.file-no-follow@55bc744f
#[cfg(unix)]
#[test]
fn the_walk_follows_no_symbolic_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("notes");
    write(dir.path(), "secret/keys.md", "outside the root");
    write(&root, "kept.md", "kept");
    std::os::unix::fs::symlink(dir.path().join("secret"), root.join("linked-dir")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret/keys.md"), root.join("linked.md")).unwrap();
    let (rows, _, declined) = read(&source(dir.path(), json!({"root": "notes"})), None).unwrap();
    assert_eq!(keys(&rows), [("kept".to_string(), 1, false)]);
    assert_eq!(declined, json!({}), "a link is not declined, it is never reached");
}

/// The `file` source walks `root`, resolved against the project directory, into one `documents` table: `.md` and
/// `.markdown` as notes, `.txt` as plain text and `.pdf` as PDF pages. Any other extension is declined unopened.
// spec: connector.source.file-source@35ae5d21
#[test]
fn notes_text_and_pdfs_land_and_every_other_extension_is_declined_by_extension() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("docs");
    write(&root, "a.md", "# A\nbody");
    write(&root, "b.markdown", "B body");
    write(&root, "c.txt", "plain");
    write(&root, "d.pdf", "%PDF-stub\npage one\u{c}page two");
    write(&root, "e.docx", "PK");
    write(&root, "f.png", "img");
    write(&root, "g.PNG", "img");
    write(&root, "Makefile", "all:");
    let mut s = source(dir.path(), json!({"root": "docs"}));
    let (rows, _, declined) = read(&s, None).unwrap();
    let kinds: Vec<(&str, &str)> = rows.iter().map(|r| (r["path"].as_str().unwrap(), r["kind"].as_str().unwrap())).collect();
    assert_eq!(kinds, [("a.md", "markdown"), ("b.markdown", "markdown"), ("c.txt", "text"), ("d.pdf", "pdf"), ("d.pdf", "pdf")]);
    assert_eq!(declined, json!({"": 1, "docx": 1, "png": 2}));
    // The pull carries the tally beside its total as the skipped count.
    let request = PullRequest { step_label: "pull-0".into(), position: None, idempotency_key: "k".into() };
    let pull: Value = serde_json::from_slice(&s.pull(&request, &Never).unwrap()).unwrap();
    assert_eq!((pull["skipped"].clone(), pull["declined"].clone(), pull["more"].clone()), (json!(4), json!({"": 1, "docx": 1, "png": 2}), json!(false)));
    assert_eq!((pull["types"]["ordinal"].clone(), pull["types"]["removed"].clone(), pull["types"]["text"].clone()), (json!("int64"), json!("boolean"), json!("utf8")));
    // An absolute root reads from where it names.
    let abs = source(Path::new("/nonexistent"), json!({"root": root.display().to_string()}));
    assert_eq!(read(&abs, None).unwrap().0.len(), 5);
    // A root naming no directory is a configuration fault.
    let f = read(&source(dir.path(), json!({"root": "missing"})), None).unwrap_err();
    assert_eq!(f.tag, FailureTag::Config, "{f}");
}

/// A root-relative path is read when it matches an `include` glob, or none is declared, and matches no `exclude`
/// glob. `*` and `?` match within one segment; a `**` segment matches any number.
// spec: connector.source.file-globs@7c0ba4a7
#[test]
fn include_and_exclude_globs_select_root_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    for p in ["top.md", "guides/one.md", "guides/deep/two.md", "guides/deep/notes.txt", "drafts/wip.md", "policies/p1.txt", "policies/p22.txt"] {
        write(dir.path(), p, "x");
    }
    let paths = |config: Value| -> Vec<String> {
        let (rows, _, _) = read(&source(dir.path(), config), None).unwrap();
        rows.iter().map(|r| r["path"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(paths(json!({"root": ".", "include": ["**/*.md"], "exclude": ["drafts/**"]})), ["guides/deep/two.md", "guides/one.md", "top.md"]);
    assert_eq!(paths(json!({"root": ".", "include": ["*.md"]})), ["top.md"], "`*` stays within one segment");
    assert_eq!(paths(json!({"root": ".", "include": ["policies/p?.txt"]})), ["policies/p1.txt"]);
    assert_eq!(paths(json!({"root": ".", "include": ["guides/**"], "exclude": ["**/*.txt"]})), ["guides/deep/two.md", "guides/one.md"]);
    for bad in [json!({"root": ".", "include": ["/abs/**"]}), json!({"root": ".", "exclude": ["../up"]}), json!({"root": ".", "include": "*.md"})] {
        assert!(FileConfig::parse(&bad).is_err(), "{bad}");
    }
}

/// A `file` table other than `documents` refuses as {{connector.source.table-unmatched}}, ahead of the walk.
// spec: connector.source.file-table-unmatched@5f152df5
#[test]
fn a_table_other_than_documents_refuses() {
    let config = FileConfig::parse(&json!({"root": "nowhere"})).unwrap();
    assert!(config.table("documents").is_ok());
    let refused = config.table("pages").unwrap_err();
    assert!(matches!(refused, ConnectorError::ConnectorTableUnmatched(_)), "{refused}");
}

/// A document source lands one row per page, or one row per heading past a length threshold. With a base URL, each
/// row links to its own page anchor.
// spec: connector.source.document-grain@efa4b1d0
#[test]
fn a_note_lands_whole_or_by_heading_and_a_pdf_by_page_each_linked_to_its_anchor() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "guides/Long Guide.md", long_note());
    write(dir.path(), "short.md", "# Short\n\nOne paragraph.\n");
    write(dir.path(), "report.pdf", "%PDF-stub\nfirst\u{c}\u{c}third");
    let (rows, _, _) = read(&source(dir.path(), json!({"root": ".", "base_url": "https://handbook.example.org/"})), None).unwrap();
    let guide: Vec<(u64, Value, Value)> = rows.iter().filter(|r| r["slug"] == json!("guides/long-guide")).map(|r| (r["ordinal"].as_u64().unwrap(), r["heading"].clone(), r["url"].clone())).collect();
    let base = "https://handbook.example.org/guides/long-guide";
    assert_eq!(
        guide,
        [
            (1, Value::Null, json!(base)),
            (2, json!("Guide"), json!(format!("{base}#guide"))),
            (3, json!("Setup"), json!(format!("{base}#setup"))),
            (4, json!("Setup"), json!(format!("{base}#setup-1"))),
        ]
    );
    assert!(row(&rows, "guides/long-guide", 3)["text"].as_str().unwrap().contains("# not a heading"), "a fenced line opens no section");
    assert_eq!(row(&rows, "short", 1)["url"], json!("https://handbook.example.org/short"));
    let pages: Vec<(Value, Value)> = rows.iter().filter(|r| r["slug"] == json!("report")).map(|r| (r["text"].clone(), r["url"].clone())).collect();
    assert_eq!(
        pages,
        [
            (json!("first"), json!("https://handbook.example.org/report#page=1")),
            (json!(""), json!("https://handbook.example.org/report#page=2")),
            (json!("third"), json!("https://handbook.example.org/report#page=3")),
        ],
        "an empty page keeps its place so ordinals stay page numbers"
    );
}

/// A note whose body past its frontmatter exceeds 8192 B lands one row per heading section, the text before its
/// first heading as its own row; a shorter note and a text file land one row.
// spec: connector.source.heading-threshold@a3c515f3
#[test]
fn a_note_splits_by_heading_only_past_the_threshold() {
    assert_eq!(CHUNK_BYTES, 8192);
    let dir = tempfile::tempdir().unwrap();
    let body = |n: usize| format!("# One\n{}\n# Two\nend\n", "x".repeat(n));
    let at = body(0).len();
    write(dir.path(), "at.md", format!("---\ntag: t\n---\n{}", body(CHUNK_BYTES - at)));
    write(dir.path(), "over.md", format!("---\ntag: t\n---\n{}", body(CHUNK_BYTES - at + 1)));
    write(dir.path(), "plain.txt", format!("# One\n{}\n# Two\n", "x".repeat(CHUNK_BYTES * 2)));
    let (rows, _, _) = read(&source(dir.path(), json!({"root": "."})), None).unwrap();
    assert_eq!(keys(&rows), [("at".to_string(), 1, false), ("over".to_string(), 1, false), ("over".to_string(), 2, false), ("plain".to_string(), 1, false)]);
    assert_eq!(row(&rows, "over", 2)["text"], json!("# Two\nend"));
}

/// A document row's identity is a slug plus an ordinal, chunked or not.
// spec: connector.source.document-identity@714866a2
#[test]
fn a_note_crossing_the_threshold_keeps_its_first_rows_identity() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Team/Plan.md", "# Plan\n\nShort.\n");
    let s = source(dir.path(), json!({"root": "."}));
    let (first, at, _) = read(&s, None).unwrap();
    assert_eq!(keys(&first), [("team/plan".to_string(), 1, false)]);
    write(dir.path(), "Team/Plan.md", format!("# Plan\n\n{}\n\n## Budget\n\nNumbers.\n", "Long. ".repeat(CHUNK_BYTES / 4)));
    let (second, _, _) = read(&s, Some(&at)).unwrap();
    assert_eq!(keys(&second), [("team/plan".to_string(), 1, false), ("team/plan".to_string(), 2, false)], "ordinal 1 is the same row, now the first section");
}

/// A document's slug is its root-relative path without extension, lowercased, each run of characters other than ASCII
/// letters and digits in a segment folded to `-`. Two files folding to one slug fail the read naming both.
// spec: connector.source.document-slug@0775cc75
#[test]
fn a_slug_folds_the_path_and_a_collision_names_both_files() {
    use contextful_connectors::file::slug;
    assert_eq!(slug("Finance/Q3 Board -- Review.md"), "finance/q3-board-review");
    assert_eq!(slug("notes/v1.2.release.txt"), "notes/v1-2-release");
    assert_eq!(slug("README"), "readme");
    assert_eq!(slug("Ünïcode/Été.md"), "n-code/t");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Plan.md", "a");
    write(dir.path(), "plan.txt", "b");
    let f = read(&source(dir.path(), json!({"root": "."})), None).unwrap_err();
    assert!(f.message.contains("`Plan.md`") && f.message.contains("`plan.txt`") && f.message.contains("`plan`"), "{f}");
    assert!(f.deterministic, "{f}");
}

/// A document row carries slug, ordinal, root-relative path, kind, title, heading, text, url, the file's SHA-256 and a
/// `removed` flag. The title is the frontmatter `title`, else the first level-one heading, else the file stem.
// spec: connector.source.document-columns@4ca4ddb8
#[test]
fn a_row_carries_its_columns_and_a_title_from_frontmatter_heading_or_stem() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let declared = "---\ntitle: \"Declared Title\"\n---\n# Heading Title\n";
    write(dir.path(), "a.md", declared);
    write(dir.path(), "b.md", "intro\n\n# Heading Title\n");
    write(dir.path(), "c-notes.md", "## Only a subheading\n");
    write(dir.path(), "d.md", "---\nTitle: Shouted\n---\n# Heading Title\n");
    let (rows, _, _) = read(&source(dir.path(), json!({"root": "."})), None).unwrap();
    let titles: Vec<&str> = rows.iter().map(|r| r["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Declared Title", "Heading Title", "c-notes", "Shouted"]);
    assert!(row(&rows, "d", 1).get("Title").is_none(), "a declared `Title` is the title, not a column");
    let a = row(&rows, "a", 1);
    let mut columns: Vec<&str> = a.keys().map(String::as_str).collect();
    columns.sort();
    assert_eq!(columns, ["heading", "kind", "ordinal", "path", "removed", "sha256", "slug", "text", "title", "url"], "a declared title fills the column, not a second one");
    let digest: String = Sha256::digest(declared.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!((a["sha256"].clone(), a["text"].clone(), a["url"].clone(), a["heading"].clone()), (json!(digest), json!("# Heading Title"), Value::Null, Value::Null));
}

/// Each other frontmatter key lands lowercased as a string column on every row of its note: a scalar as its text, a list as a
/// JSON array.
// spec: connector.source.frontmatter-columns@bc706cbc
#[test]
fn frontmatter_keys_land_as_string_columns_on_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let front = "---\nowner: Ops\nreviewed: 2031-03-01\ntags: [onboarding, \"set, up\"]\naliases:\n  - first\n  - second\nempty:\n---\n";
    write(dir.path(), "guide.md", format!("{front}{}", long_note()));
    write(dir.path(), "bare.md", "no frontmatter");
    write(dir.path(), "shouted.md", "---\nOwner: Finance\n---\nbody");
    let (rows, _, _) = read(&source(dir.path(), json!({"root": "."})), None).unwrap();
    assert_eq!(row(&rows, "shouted", 1)["owner"], json!("Finance"), "a key lands under its lowercased name");
    assert!(rows.iter().all(|r| !r.contains_key("Owner")), "`owner` and `Owner` share one column");
    for r in rows.iter().filter(|r| r["slug"] == json!("guide")) {
        assert_eq!(
            (r["owner"].clone(), r["reviewed"].clone(), r["tags"].clone(), r["aliases"].clone(), r["empty"].clone()),
            (json!("Ops"), json!("2031-03-01"), json!("[\"onboarding\",\"set, up\"]"), json!("[\"first\",\"second\"]"), Value::Null)
        );
    }
    assert_eq!(rows.iter().filter(|r| r["slug"] == json!("guide")).count(), 4);
    assert_eq!(row(&rows, "bare", 1)["owner"], Value::Null, "every row of one pull carries every frontmatter column");
}

/// A nested map, a block scalar, a key carrying the reserved producer prefix, or, compared without case, a repeated
/// key or a key other than `title` naming a column the source lands, in a note's frontmatter raises
/// `ConnectorFrontmatterRejected`.
// spec: connector.source.frontmatter-shape@d5e4355e
#[test]
fn a_nested_map_block_scalar_or_reserved_key_in_frontmatter_refuses() {
    for (front, why) in [
        ("owner:\n  name: Ops\n", "nested map"),
        ("owner: { name: Ops }\n", "nested map"),
        ("tags:\n  - name: one\n", "nested value"),
        ("summary: |\n  line one\n", "block scalar"),
        ("summary: >-\n  folded\n", "block scalar"),
        ("_taint: trusted\n", "reserved producer prefix"),
        ("text: replaced\n", "names a column"),
        ("Text: replaced\n", "names a column"),
        ("SLUG: other\n", "names a column"),
        ("owner: a\nowner: b\n", "declared twice"),
        ("owner: a\nOwner: b\n", "declared twice"),
        ("title: a\nTitle: b\n", "declared twice"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "kept.md", "kept");
        write(dir.path(), "notes/bad.md", format!("---\n{front}---\nbody\n"));
        let f = read(&source(dir.path(), json!({"root": "."})), None).unwrap_err();
        assert!(f.message.starts_with("ConnectorFrontmatterRejected") && f.message.contains("notes/bad.md") && f.message.contains(why), "{front}: {f}");
        assert_eq!(f.tag, FailureTag::Permanent, "{front}");
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "open.md", "---\nowner: Ops\nbody without a close\n");
    let f = read(&source(dir.path(), json!({"root": "."})), None).unwrap_err();
    assert!(f.message.starts_with("ConnectorFrontmatterRejected") && f.message.contains("never closes"), "{f}");
}

/// A compound-binary office container, detected by an office extension or by magic bytes under an extension the
/// source reads, raises `ConnectorConversionRequired` naming the conversion command.
// spec: connector.source.conversion-required@6c075c90
#[test]
fn a_compound_binary_container_refuses_by_extension_or_magic_naming_the_command() {
    const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    for (path, body, target) in [
        ("Board/minutes.doc", b"not even ole".to_vec(), "pdf"),
        ("Board/ledger.XLS", b"x".to_vec(), "xlsx"),
        ("Board/renamed.txt", [MAGIC.as_slice(), b"rest"].concat(), "pdf"),
        ("Board/renamed.md", [MAGIC.as_slice(), b"rest"].concat(), "pdf"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "first");
        write(dir.path(), path, body);
        let f = read(&source(dir.path(), json!({"root": "."})), None).unwrap_err();
        assert!(f.message.starts_with("ConnectorConversionRequired") && f.message.contains(path), "{path}: {f}");
        assert!(f.message.contains(&format!("soffice --headless --convert-to {target}")), "{path}: {f}");
    }
    // A compound container under an extension the source neither reads nor converts is declined unopened.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "first");
    write(dir.path(), "Thumbs.db", [MAGIC.as_slice(), b"rest"].concat());
    write(dir.path(), "mail/note.msg", [MAGIC.as_slice(), b"rest"].concat());
    let (rows, _, declined) = read(&source(dir.path(), json!({"root": "."})), None).unwrap();
    assert_eq!(keys(&rows), [("a".to_string(), 1, false)]);
    assert_eq!(declined, json!({"db": 1, "msg": 1}));
}

/// A compiled-in source decodes behind {{run.land.parse-boundary}}.
// spec: connector.source.parse-containment@51532158
#[cfg(unix)]
#[test]
fn a_pdf_decodes_in_a_child_process_and_its_crash_fails_the_read_alone() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.pdf", "%PDF-1.4");
    let with = |script: &str| {
        let config = FileConfig::parse(&json!({"root": "."})).unwrap();
        let worker: Arc<dyn PageDecoder> = Arc::new(Boundary::new("/bin/sh", &["-c", script]));
        FileSource::new(config, dir.path(), Some(worker))
    };
    let (rows, _, _) = read(&with("cat >/dev/null; echo '[\"one\",\"two\"]'"), None).unwrap();
    assert_eq!(keys(&rows), [("a".to_string(), 1, false), ("a".to_string(), 2, false)]);
    let f = read(&with("kill -9 $$"), None).unwrap_err();
    assert!(f.message.starts_with("PipelineParseCrashed") && f.message.contains("a.pdf"), "{f}");
}

/// The `file` position maps each landed path to its digest and row count. A matching digest lands nothing, a changed
/// file re-lands whole and tombstones ordinals past its new count, and a vanished file tombstones every row.
// spec: connector.source.file-position@c10aa1b7
#[test]
fn a_second_read_relands_what_changed_and_tombstones_what_left() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "kept.md", "kept");
    write(dir.path(), "shrinks.pdf", "%PDF-stub\none\u{c}two\u{c}three");
    write(dir.path(), "gone.txt", "bye");
    let s = source(dir.path(), json!({"root": "."}));
    let (_, at, _) = read(&s, None).unwrap();
    assert_eq!(at["files"]["shrinks.pdf"]["rows"], json!(3));

    let (unchanged, again, _) = read(&s, Some(&at)).unwrap();
    assert!(unchanged.is_empty(), "{unchanged:?}");
    assert_eq!(again, at);

    write(dir.path(), "shrinks.pdf", "%PDF-stub\nONE");
    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    let (rows, after, _) = read(&s, Some(&at)).unwrap();
    assert_eq!(keys(&rows), [("shrinks".to_string(), 1, false), ("shrinks".to_string(), 2, true), ("shrinks".to_string(), 3, true), ("gone".to_string(), 1, true)]);
    let tomb = row(&rows, "gone", 1);
    assert_eq!((tomb["path"].clone(), tomb["text"].clone(), tomb["sha256"].clone()), (json!("gone.txt"), Value::Null, Value::Null));
    assert_eq!(after["files"].as_object().unwrap().keys().collect::<Vec<_>>(), ["kept.md", "shrinks.pdf"]);
}

/// A file renamed onto its old slug re-lands under its new path, and the old path tombstones only ordinals past the
/// renamed file's row count.
// spec: connector.source.file-rename@d4f061d4
#[test]
fn a_rename_onto_the_same_slug_keeps_the_renamed_rows() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", "first");
    write(dir.path(), "My Report.pdf", "%PDF-stub\none\u{c}two\u{c}three");
    let s = source(dir.path(), json!({"root": "."}));
    let (_, at, _) = read(&s, None).unwrap();

    std::fs::rename(dir.path().join("a.txt"), dir.path().join("a.md")).unwrap();
    std::fs::remove_file(dir.path().join("My Report.pdf")).unwrap();
    write(dir.path(), "my-report.txt", "one page now");
    let (rows, after, _) = read(&s, Some(&at)).unwrap();
    let mut got = keys(&rows);
    got.sort();
    assert_eq!(
        got,
        [("a".to_string(), 1, false), ("my-report".to_string(), 1, false), ("my-report".to_string(), 2, true), ("my-report".to_string(), 3, true)]
    );
    assert_eq!(row(&rows, "a", 1)["path"], json!("a.md"));
    assert_eq!(row(&rows, "my-report", 1)["path"], json!("my-report.txt"));
    assert_eq!(after["files"].as_object().unwrap().keys().collect::<Vec<_>>(), ["a.md", "my-report.txt"]);
}

/// A file over 64 MiB is declined, and none of it past the leading bytes announcing its format is read.
// spec: connector.source.file-cap@cac3db47
#[test]
fn a_file_over_the_cap_is_declined_unread() {
    assert_eq!(MAX_FILE_BYTES, 64 * 1024 * 1024);
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "small.md", "small");
    // A sparse file past the cap, whose body would not decode as text.
    let big = std::fs::File::create(dir.path().join("huge.md")).unwrap();
    big.set_len(MAX_FILE_BYTES + 1).unwrap();
    drop(big);
    std::fs::write(dir.path().join("at-cap.txt"), "x".repeat(MAX_FILE_BYTES as usize)).unwrap();
    let (rows, _, declined) = read(&source(dir.path(), json!({"root": "."})), None).unwrap();
    assert_eq!(rows.iter().map(|r| r["path"].as_str().unwrap()).collect::<Vec<_>>(), ["at-cap.txt", "small.md"]);
    assert_eq!(declined, json!({"md": 1}));
}

/// A PDF the walk reaches in a build without the PDF decoder fails the read as a configuration fault naming the feature
/// to rebuild with.
// spec: connector.source.file-pdf-absent@34ba746d
#[test]
fn a_pdf_without_a_decoder_names_the_feature_to_rebuild_with() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "note");
    write(dir.path(), "b.pdf", "%PDF-stub\none");
    let s = FileSource::new(FileConfig::parse(&json!({"root": "."})).unwrap(), dir.path(), None);
    let f = read(&s, None).unwrap_err();
    assert_eq!(f.tag, FailureTag::Config, "{f}");
    assert!(f.message.contains("b.pdf") && f.message.contains("--features pdf"), "{f}");
}

/// A text file that is not UTF-8 is unreadable input naming the path and the byte.
#[test]
fn a_text_file_that_is_not_utf8_is_unreadable_input() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "latin.txt", b"caf\xe9");
    let f = read(&source(dir.path(), json!({"root": "."})), None).unwrap_err();
    assert!(f.message.starts_with("PipelineUnreadableInput") && f.message.contains("latin.txt") && f.message.contains("byte 3"), "{f}");
}
