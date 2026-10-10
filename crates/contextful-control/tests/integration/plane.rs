//! `topology.package` for the control profile: the configuration as one CRDT document,
//! the operator attestation as identity, and canonical TOML claimed on apply.

use contextful_control::{apply, canonical, Attestation, ConfigDoc, APPLY_TARGET};
use contextful_core::surface::SurfaceError;
use contextful_snapshot::{ControlError, SnapshotDir};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const SECRET: &str = "console-secret";
const NOW: i64 = 1_800_000_000;

const BASE: &str = "\
# Team configuration
[[source]]
id = \"filings\"
url = \"https://example.org/filings\"

[[model]]
id = \"embed\"
endpoint = \"https://models.example.org/v1\"

[policy]
default = \"deny\"
";

/// The console's signature over one apply of `document` by `operator`.
fn attest(operator: &str, nonce: &str, at: i64, document: &str) -> Attestation {
    let digest = format!("{:x}", Sha256::digest(document.as_bytes()));
    let message = format!("POST\n{APPLY_TARGET}\n{digest}\n{operator}\n{at}\n{nonce}");
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(message.as_bytes());
    let signature = mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    Attestation { operator: operator.into(), time: at.to_string(), nonce: nonce.into(), signature }
}

fn nonce(n: u8) -> String {
    format!("{n:032x}")
}

/// The control profile's team state is the configuration — its ingest sources, models and access policy — held as one
/// `loro` document whose replicas merge concurrent edits; that document is the edit-time copy of what apply claims.
// spec: topology.package.control-team-state@deeaf2ad
#[test]
fn replicas_of_the_configuration_document_merge_concurrent_edits() {
    let alice = ConfigDoc::new(1).unwrap();
    alice.edit(BASE).unwrap();
    let bob = ConfigDoc::load(2, &alice.export().unwrap()).unwrap();
    assert_eq!(bob.text(), BASE);

    alice.edit(&BASE.replace("default = \"deny\"", "default = \"deny\"\nreaders = [\"analysts\"]")).unwrap();
    bob.edit(&BASE.replace("id = \"embed\"", "id = \"embed\"\ndimensions = 768")).unwrap();
    alice.merge(&bob.export().unwrap()).unwrap();
    bob.merge(&alice.export().unwrap()).unwrap();

    assert_eq!(alice.text(), bob.text(), "replicas converge");
    let merged = alice.text();
    assert!(merged.contains("readers = [\"analysts\"]") && merged.contains("dimensions = 768"), "{merged}");

    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let document = canonical(&merged).unwrap();
    let v = apply(&snaps, &alice, None, SECRET, &attest("alice", &nonce(1), NOW, &document), NOW).unwrap();
    assert_eq!(snaps.read(v).unwrap(), document, "apply claims the document the replicas hold");
}

/// Apply materializes canonical TOML: every table's keys in byte order, tables after the values they nest in, quoting
/// and spacing the format-preserving editor chooses, and every comment kept; canonicalizing canonical text changes no
/// byte.
// spec: topology.package.canonical-toml@3cb3e90d
#[test]
fn apply_claims_canonical_toml_with_every_comment_kept() {
    let messy = "\
[policy]   # who reads
default='deny'
# the readers group
readers = [ 'analysts' ]

# Sources first written by hand
[[source]]
url    =   \"https://example.org/filings\"
id = 'filings' # the landing table

title = \"Team\"
";
    let a = canonical(messy).unwrap();
    assert_eq!(canonical(&a).unwrap(), a, "canonical text is a fixed point");
    for comment in ["# who reads", "# the readers group", "# Sources first written by hand", "# the landing table"] {
        assert!(a.contains(comment), "{comment} survives: {a}");
    }
    assert!(a.find("default").unwrap() < a.find("readers").unwrap(), "keys sort within a table: {a}");
    assert!(a.find("[policy]").unwrap() < a.find("[[source]]").unwrap(), "tables sort by key: {a}");

    let equal_one = "title = 'Team'\n[policy] # who reads\ndefault='deny'\n# the readers group\nreaders = [ 'analysts' ]\n";
    let equal_two = "title = \"Team\"\n\n[policy]    # who reads\n# the readers group\nreaders = [\"analysts\"]\ndefault = \"deny\"\n";
    let one = canonical(equal_one).unwrap();
    assert_eq!(one, canonical(equal_two).unwrap(), "equal configurations claim identical bytes");
    assert_eq!(one, "title = \"Team\"\n\n[policy] # who reads\ndefault = \"deny\"\n# the readers group\nreaders = [\"analysts\"]\n");

    assert_eq!(
        canonical("[b]\nz = 0x10\n[a.c]\nx = { q = 1, p = 2 }\n[a]\ny = 2\n").unwrap(),
        "[a]\ny = 2\n\n[a.c]\nx = { p = 2, q = 1 }\n\n[b]\nz = 16\n",
        "a table follows the values it nests in"
    );

    let doc = ConfigDoc::new(1).unwrap();
    doc.edit(equal_one).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let v = apply(&snaps, &doc, None, SECRET, &attest("alice", &nonce(1), NOW, &one), NOW).unwrap();
    assert_eq!(snaps.read(v).unwrap(), one, "apply claims the canonical text, not the edit-time spelling");

    let broken = ConfigDoc::new(3).unwrap();
    broken.edit("[policy\ndefault = ").unwrap();
    let err = apply(&snaps, &broken, Some(v), SECRET, &attest("alice", &nonce(2), NOW, "[policy\ndefault = "), NOW).unwrap_err();
    assert!(matches!(err, ControlError::Surface(SurfaceError::ApplyValidationRefused(_))), "{err:?}");
    assert_eq!(snaps.current().unwrap(), Some(v), "a refused document claims no version");
}

/// The control profile identifies an operator by {{surface.apply.operator-attestation}} alone, refusing an apply
/// without one, and keeps no account store.
// spec: topology.package.control-identity@0d2d5f70
#[test]
fn apply_admits_only_a_fresh_unreplayed_operator_attestation() {
    let doc = ConfigDoc::new(1).unwrap();
    doc.edit(BASE).unwrap();
    let document = canonical(BASE).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let snaps = SnapshotDir::open(dir.path());
    let invalid = |err: ControlError| matches!(err, ControlError::OperatorAttestationInvalid(_));

    let mut forged = attest("alice", &nonce(1), NOW, &document);
    forged.signature = "0".repeat(64);
    assert!(invalid(apply(&snaps, &doc, None, SECRET, &forged, NOW).unwrap_err()));
    assert!(invalid(apply(&snaps, &doc, None, SECRET, &attest("alice", &nonce(1), NOW - 120, &document), NOW).unwrap_err()));
    assert!(invalid(apply(&snaps, &doc, None, SECRET, &attest("alice", &nonce(1), NOW, "another document"), NOW).unwrap_err()));
    assert!(invalid(apply(&snaps, &doc, None, "another secret", &attest("alice", &nonce(1), NOW, &document), NOW).unwrap_err()));
    assert_eq!(snaps.current().unwrap(), None, "a refused attestation claims nothing");

    let signed = attest("alice", &nonce(1), NOW, &document);
    let v = apply(&snaps, &doc, None, SECRET, &signed, NOW).unwrap();
    assert_eq!(v, 1);
    assert!(invalid(apply(&snaps, &doc, Some(v), SECRET, &signed, NOW).unwrap_err()), "a replayed nonce is refused");

    let stale = apply(&snaps, &doc, Some(0), SECRET, &attest("bob", &nonce(2), NOW, &document), NOW).unwrap_err();
    assert!(matches!(stale, ControlError::Surface(SurfaceError::ManifestVersionConflict(_))), "{stale:?}");
    assert_eq!(apply(&snaps, &doc, Some(v), SECRET, &attest("bob", &nonce(3), NOW, &document), NOW).unwrap(), 2);

    let entries: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!entries.is_empty(), "the applied versions left no snapshot");
    assert!(!entries.iter().any(|name| name.contains("account") || name.contains("user")), "{entries:?}");
}
