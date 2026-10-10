//! The handshake's required-face check and tool registration by face scope.

use super::strings;
use contextful_core::enforce::EnforceError;
use contextful_core::read::face::{read_backend, register_tool, require, BuildIdentity, FaceScope, ToolKind, TOOLS};
use contextful_core::read::ReadError;

#[test]
fn citation_resolution_belongs_to_the_closed_read_tool_set() {
    assert!(contextful_core::read::face::TOOLS.contains(&"context.reference"));
    assert_eq!(register_tool(FaceScope::Organization, "context.reference", ToolKind::Read), Ok(()));
}

/// A client passing `require: [...]` is refused ahead of its first read with `RequiredFaceAbsent` for any name outside the reported set. An engine reporting no set satisfies no requirement.
// spec: read.embed.required-face@2167a7a9
#[test]
fn a_requirement_outside_the_reported_set_is_refused() {
    let id = BuildIdentity { backends: strings(&["duckdb", "fts"]), connectors: Vec::new(), faces: Vec::new() };
    assert_eq!(require(Some(&id), &strings(&["duckdb", "fts"])), Ok(()));
    match require(Some(&id), &strings(&["duckdb", "hnsw", "http"])) {
        Err(ReadError::RequiredFaceAbsent(why)) => assert!(why.contains("hnsw") && why.contains("http"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(require(None, &strings(&["duckdb"])), Err(ReadError::RequiredFaceAbsent(_))));
    assert_eq!(require(None, &[]), Ok(()));
}

/// Without the embedded SQL engine linked, every read tool raises `ReadBackendAbsent` rather than answering from a narrower path.
// spec: read.embed.absent-read-backend@b0029a31
#[test]
fn a_build_without_the_sql_engine_refuses_every_read_tool() {
    let linked = BuildIdentity { backends: strings(&["duckdb", "fts"]), connectors: Vec::new(), faces: Vec::new() };
    let unlinked = BuildIdentity { backends: strings(&["fts", "hnsw"]), connectors: Vec::new(), faces: strings(&["http"]) };
    for tool in TOOLS.iter().copied().chain(["notes_for"]) {
        assert_eq!(read_backend(&linked, tool), Ok(()), "{tool}");
        match read_backend(&unlinked, tool) {
            Err(e @ ReadError::ReadBackendAbsent(_)) => {
                assert_eq!(e.identifier(), "ReadBackendAbsent");
                assert!(e.to_string().contains(tool) && e.to_string().contains("duckdb"), "{e}");
            }
            other => panic!("{tool}: {other:?}"),
        }
    }
}

/// An organization-wide face serves the read subset of the tool surface; conversational writes are server-authored.
// spec: authority.resist.read-only-face@9b23387c
#[test]
fn an_organization_face_registers_read_tools() {
    assert_eq!(register_tool(FaceScope::Organization, "context.query", ToolKind::Read), Ok(()));
    assert_eq!(register_tool(FaceScope::Personal, "memory.write", ToolKind::Write), Ok(()));
}

/// Registering a write tool on an organization-wide face raises `EnforceWriteOnReadOnlyFace`.
// spec: authority.resist.write-tool@35022f1c
#[test]
fn a_write_tool_on_an_organization_face_is_refused() {
    match register_tool(FaceScope::Organization, "memory.write", ToolKind::Write) {
        Err(EnforceError::WriteOnReadOnlyFace(why)) => assert!(why.contains("memory.write"), "{why}"),
        other => panic!("{other:?}"),
    }
}
