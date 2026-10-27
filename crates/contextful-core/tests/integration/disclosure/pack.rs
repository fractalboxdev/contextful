//! `disclosure.pack`: the `[pack.<name>]` block, its access mappings, its container
//! allowlist, and the refusals at load and at diagnose.

use contextful_core::disclosure::declare::{Family, Fidelity};
use contextful_core::disclosure::pack::{Evaluate, Pack, PackError};
use contextful_core::disclosure::VisibilityError;
use contextful_core::pipeline::declare::ManifestFile;

const PACK: &str = r#"
[pack.wiki]
tables    = ["pages", "drafts"]
excluded  = ["drafts"]
allowlist = { containers = ["eng", "ops"], evaluate = "all-of" }
base_url  = "https://wiki.example/api"
page_size = 100

[pack.wiki.access.pages]
resource_key   = "page_id"
resource_kind  = "page"
family         = "item-exception"
fidelity       = "coarse"
container_key  = "space_id"
principal_kind = { user = "user", group = "group", workspace = "org", anyone = "public" }
level          = { viewer = "read", editor = "write" }
"#;

fn visibility(text: &str) -> VisibilityError {
    match Pack::parse_manifest(text) {
        Err(PackError::Visibility(e)) => e,
        other => panic!("expected a visibility refusal, got {other:?}"),
    }
}

/// A refusal heads its text with `identifier` and names each of `names`.
fn refused(e: &VisibilityError, identifier: &str, names: &[&str]) {
    assert_eq!(e.identifier(), identifier);
    let shown = e.to_string();
    assert!(shown.starts_with(identifier), "{shown}");
    for n in names {
        assert!(shown.contains(n), "`{n}` missing from {shown}");
    }
}

/// A `[pack.<name>]` block names the `tables` it lands, its `excluded` tables, one `access.<table>` mapping per mapped table, and an optional container `allowlist` of `containers` with `evaluate = "all-of"`.
// spec: disclosure.pack.pack-block@ab26cae5
#[test]
fn a_pack_block_names_its_tables_mappings_and_allowlist() {
    let packs = Pack::parse_manifest(PACK).unwrap();
    assert_eq!(packs.len(), 1);
    let wiki = &packs[0];
    assert_eq!(wiki.name, "wiki");
    assert_eq!(wiki.tables, ["pages", "drafts"]);
    assert_eq!(wiki.excluded, ["drafts"]);
    let allow = wiki.allowlist.as_ref().unwrap();
    assert_eq!(allow.containers, ["eng", "ops"]);
    assert_eq!(allow.evaluate, Evaluate::AllOf);
    assert!(wiki.access.contains_key("pages"));
    wiki.diagnose().unwrap();

    // An allowlist naming no evaluation is all-of; a manifest with no pack holds none.
    let bare = PACK.replace(", evaluate = \"all-of\"", "");
    assert_eq!(Pack::parse_manifest(&bare).unwrap()[0].allowlist.as_ref().unwrap().evaluate, Evaluate::AllOf);
    assert!(Pack::parse_manifest("[pipeline]\n").unwrap().is_empty());

    // Packs across a declaration set: a name declared in two files refuses.
    let files = [ManifestFile { path: "contextful.toml".into(), text: "[project]\nname = \"w\"\n".into() }, ManifestFile { path: "pipelines/wiki.toml".into(), text: PACK.into() }];
    assert_eq!(Pack::parse_set(&files).unwrap().len(), 1);
    let twice = [ManifestFile { path: "contextful.toml".into(), text: PACK.into() }, ManifestFile { path: "pipelines/wiki.toml".into(), text: PACK.into() }];
    assert!(matches!(Pack::parse_set(&twice), Err(PackError::Malformed(_))));
    for bad in ["[pack]\nwiki = 1\n", "[pack.wiki]\ntables = \"pages\"\n", "[pack.wiki]\ntables = [\"pages\"]\nexcluded = [\"pages\"]\n[pack.wiki.access.pages]\nresource_key = \"id\"\nfamily = \"item-exception\"\n"] {
        assert!(matches!(Pack::parse_manifest(bad), Err(PackError::Malformed(_))), "{bad}");
    }
}

/// An access mapping holds {{type:contextful_core::disclosure::pack::RawMapping}} and no other key.
// spec: disclosure.pack.mapping-keys@e52eaea9
#[test]
fn an_access_mapping_holds_the_schema_keys_alone() {
    let wiki = Pack::parse_manifest(PACK).unwrap().remove(0);
    let m = &wiki.access["pages"];
    assert_eq!(m.resource_key, "page_id");
    assert_eq!(m.resource_kind.as_deref(), Some("page"));
    assert_eq!(m.family, Family::ItemException);
    assert_eq!(m.fidelity, Some(Fidelity::Coarse));
    assert_eq!(m.container_key.as_deref(), Some("space_id"));
    assert_eq!(m.principal_kind["anyone"], "public");
    assert_eq!(m.level["editor"], "write");

    // A key outside the schema that is not access-shaped does not load either.
    let extra = PACK.replace("level          =", "page_limit = 5\nlevel          =");
    assert!(matches!(Pack::parse_manifest(&extra), Err(PackError::Malformed(_))));
    // A mapping naming no family is refused as any mapping is.
    let familyless = PACK.replace("family         = \"item-exception\"\n", "");
    refused(&visibility(&familyless), "VisibilityFamilyUndeclared", &["pages"]);
}

/// A pack landing a table with neither an access mapping nor `excluded` raises `VisibilityMappingAbsent` at diagnose, naming the pack and the table.
// spec: disclosure.pack.mapping-absent@110eac6a
#[test]
fn a_landed_table_with_no_mapping_and_no_exclusion_is_refused_at_diagnose() {
    let text = PACK.replace("tables    = [\"pages\", \"drafts\"]", "tables    = [\"pages\", \"drafts\", \"comments\"]");
    let wiki = Pack::parse_manifest(&text).unwrap().remove(0);
    let e = wiki.diagnose().unwrap_err();
    assert_eq!(e, VisibilityError::MappingAbsent { pack: "wiki".into(), table: "comments".into() });
    refused(&e, "VisibilityMappingAbsent", &["wiki", "comments"]);

    // Excluding the table, or mapping it, satisfies diagnose.
    let excluded = text.replace("excluded  = [\"drafts\"]", "excluded  = [\"drafts\", \"comments\"]");
    Pack::parse_manifest(&excluded).unwrap()[0].diagnose().unwrap();
}

/// An access-shaped key the mapping schema does not define raises `VisibilityPackAssertsAccess` by name. Fields outside the mapping are fetch inputs and reach no authorization decision.
// spec: disclosure.pack.asserts-access@4e4893c7
#[test]
fn an_access_shaped_key_outside_the_mapping_schema_is_refused_by_name() {
    // `base_url` and `page_size` are fetch inputs and load.
    Pack::parse_manifest(PACK).unwrap();
    for (key, line, at) in [
        ("audience", "audience = \"everyone\"\n", "base_url"),
        ("visible_to", "visible_to = [\"eng\"]\n", "base_url"),
        ("share_with", "share_with = \"org\"\n", "base_url"),
        ("acl", "acl = { pages = \"public\" }\n", "base_url"),
        ("readers", "readers = [\"dana\"]\n", "level          ="),
        ("public", "public = true\n", "level          ="),
        ("viewer", "viewer = \"anyone\"\n", "base_url"),
        ("editors", "editors = [\"eng\"]\n", "base_url"),
        ("sharedWith", "sharedWith = \"org\"\n", "base_url"),
        ("collaborators", "collaborators = [\"eng\"]\n", "base_url"),
        ("adminUsers", "adminUsers = [\"dana\"]\n", "base_url"),
    ] {
        let text = PACK.replacen(at, &format!("{line}{at}"), 1);
        refused(&visibility(&text), "VisibilityPackAssertsAccess", &["wiki", key]);
    }
    // A nested table outside the mapping is read the same way.
    let nested = format!("{PACK}\n[pack.wiki.fetch]\nretries = 3\nallow_roles = [\"admin\"]\n");
    refused(&visibility(&nested), "VisibilityPackAssertsAccess", &["allow_roles"]);
    let fetch = format!("{PACK}\n[pack.wiki.fetch]\nretries = 3\n");
    Pack::parse_manifest(&fetch).unwrap();
}

/// An allowlist evaluated as any-of raises `VisibilityAllowlistAnyOf` at load.
// spec: disclosure.pack.any-of-allowlist@8f65d358
#[test]
fn an_allowlist_evaluated_as_any_of_is_refused_at_load() {
    let text = PACK.replace("evaluate = \"all-of\"", "evaluate = \"any-of\"");
    refused(&visibility(&text), "VisibilityAllowlistAnyOf", &["wiki"]);
    let other = PACK.replace("evaluate = \"all-of\"", "evaluate = \"some\"");
    assert!(matches!(Pack::parse_manifest(&other), Err(PackError::Malformed(_))));
}

/// Declaring `coarse` where the source emits no container or workspace signal raises `VisibilityUnsupportedCoarse`.
// spec: disclosure.pack.unsupported-coarse@5f8d44ef
#[test]
fn coarse_without_a_container_signal_is_refused() {
    let text = PACK.replace("container_key  = \"space_id\"\n", "");
    refused(&visibility(&text), "VisibilityUnsupportedCoarse", &["wiki", "pages"]);
    // A mirrored mapping needs no container signal.
    let mirrored = text.replace("fidelity       = \"coarse\"", "fidelity       = \"mirrored\"");
    Pack::parse_manifest(&mirrored).unwrap();
}
