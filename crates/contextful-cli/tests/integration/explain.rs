//! `contextful audit explain` through the built binary: the window replay, its coverage
//! block, the audience report, and the refusals an explanation over the chain raises.
#![cfg(feature = "data-plane")]

use contextful_policy::audit::{attr, AuditLog};
use contextful_policy::issue::SeedSigner;
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful"))
        .args(args)
        .current_dir(dir)
        .env_remove("CONTEXTFUL_TOKEN")
        .env_remove("CONTEXTFUL_ISSUER_PUBKEY")
        .output()
        .unwrap()
}

fn answer(out: &Output) -> Value {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn refusal(out: &Output) -> String {
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// An RFC 3339 UTC instant `offset` seconds from now.
fn instant(offset: i64) -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + offset;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn window(from: i64, to: i64) -> String {
    format!("{}..{}", instant(from), instant(to))
}

fn read(offset: i64, who: &str, tables: &[&str], outcome: &str) -> Value {
    json!({
        attr::READ_AT: instant(offset),
        attr::ON_BEHALF_OF: who,
        attr::AGENT: "agent://research-loop",
        attr::TABLES: tables,
        attr::POLICY: "rev-1",
        attr::OUTCOME: outcome,
        attr::ROWS: 1,
    })
}

/// A project directory whose chain holds `batch`, signed under a fresh issuer seed.
fn project(manifest: &str, batch: Vec<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join(".contextful")).unwrap();
    std::fs::write(p.join("contextful.toml"), manifest).unwrap();
    answer_text(&run(p, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let signer = SeedSigner::from_seed(&std::fs::read_to_string(p.join(".contextful/issuer.seed")).unwrap()).unwrap();
    let log = AuditLog::open(p.join(".contextful/audit"), signer).unwrap();
    log.append_all(batch).unwrap();
    log.export().unwrap();
    dir
}

fn answer_text(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const NOTES: &str = "[[pipeline.tables]]\nname = \"research/notes\"\n";

const ADA: &str = "user://ada@acme.example";

/// `--window <from>..<to>` replays each chain entry naming the subject and the table whose
/// read instant falls in the window: its seq, instant, outcome, agent and recorded
/// `contextful.*` attributes.
// spec: disclosure.explain.replay@a38c5971
#[test]
fn a_window_replays_the_subjects_reads_of_the_table_inside_it() {
    let dir = project(
        NOTES,
        vec![
            read(-7200, ADA, &["research/notes"], attr::SERVED),
            read(-600, ADA, &["research/notes"], attr::SERVED),
            read(-500, "user://bo@acme.example", &["research/notes"], attr::SERVED),
            read(-400, ADA, &["hr/salaries"], attr::REFUSED),
            read(-300, ADA, &["research/notes", "hr/salaries"], attr::REFUSED),
        ],
    );
    let out = answer(&run(
        dir.path(),
        &[
            "audit",
            "explain",
            "--project",
            "research",
            "--table",
            "research/notes",
            "--subject",
            ADA,
            "--window",
            &window(-3600, 0),
        ],
    ));
    let replay = out["replay"].as_array().unwrap();
    let seqs: Vec<u64> = replay.iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, [2, 5], "{out}");
    assert_eq!(replay[0]["outcome"], "served");
    assert_eq!(replay[1]["outcome"], "refused");
    assert_eq!(replay[0]["agent"], "agent://research-loop");
    assert_eq!(replay[0]["attributes"][attr::ROWS], 1);
    assert_eq!(replay[0]["attributes"][attr::POLICY], "rev-1");
    assert!(replay[0]["attributes"].get(attr::ON_BEHALF_OF).is_none(), "{out}");
    // No exchange policy is declared, so the decision denies under a coverage block.
    assert_eq!(out["decision"]["verdict"], "deny");
    assert!(out["coverage"].is_object());
}

/// A window holding no observations raises `VisibilityNoObservations` and states that no
/// claim is available.
// spec: disclosure.explain.empty-window@6bc03e30
#[test]
fn a_window_the_chain_holds_no_entry_in_refuses_with_no_claim() {
    let dir = project(NOTES, vec![read(-60, ADA, &["research/notes"], attr::SERVED)]);
    let err = refusal(&run(
        dir.path(),
        &[
            "audit",
            "explain",
            "--project",
            "research",
            "--table",
            "research/notes",
            "--subject",
            ADA,
            "--window",
            &window(-172_800, -86_400),
        ],
    ));
    assert!(err.contains("VisibilityNoObservations"), "{err}");
    assert!(err.contains("no claim is available"), "{err}");

    let err = refusal(&run(
        dir.path(),
        &["audit", "explain", "--project", "research", "--audience", "research/notes", "--window", &window(-172_800, -86_400)],
    ));
    assert!(err.contains("VisibilityNoObservations"), "{err}");
}

/// An explanation returning a row, field value or excerpt from the resource it decides
/// about, a replayed `contextful.result.*` attribute beyond `contextful.result.rows`
/// included, raises `VisibilityDiagnosticRow`.
// spec: disclosure.explain.no-row@110dc152
#[test]
fn a_replayed_entry_carrying_result_content_is_refused_without_echoing_it() {
    let mut leaky = read(-60, ADA, &["research/notes"], attr::SERVED);
    leaky["contextful.result.excerpt"] = json!("Solar battery storage");
    let dir = project(NOTES, vec![read(-120, ADA, &["research/notes"], attr::SERVED), leaky]);
    let err = refusal(&run(
        dir.path(),
        &[
            "audit",
            "explain",
            "--project",
            "research",
            "--table",
            "research/notes",
            "--subject",
            ADA,
            "--window",
            &window(-3600, 0),
        ],
    ));
    assert!(err.contains("VisibilityDiagnosticRow"), "{err}");
    assert!(err.contains("contextful.result.excerpt"), "{err}");
    assert!(!err.contains("Solar"), "{err}");
}

/// An explanation carries a coverage block: segments and entries verified, entries
/// carrying no read instant, window spans the chain does not reach, and each visibility
/// binding's source with its sweep watermark.
// spec: disclosure.explain.coverage@02a94767
#[test]
fn coverage_counts_the_chain_and_names_unreached_spans_and_the_binding() {
    let manifest = "[[pipeline.tables]]\nname = \"research/notes\"\n\n[pipeline.tables.visibility]\nsource = \"wiki\"\nresource_key = \"page_id\"\nfidelity = \"mirrored\"\nfamily = \"item-exception\"\n";
    let dir = project(
        manifest,
        vec![json!({ "contextful.tool": "context.query" }), read(-600, ADA, &["research/notes"], attr::SERVED)],
    );
    let out = answer(&run(
        dir.path(),
        &[
            "audit",
            "explain",
            "--project",
            "research",
            "--table",
            "research/notes",
            "--subject",
            ADA,
            "--window",
            &window(-3600, 3600),
        ],
    ));
    let c = &out["coverage"];
    assert_eq!(c["segments"], 1, "{out}");
    assert_eq!(c["entries"], 2);
    assert_eq!(c["unplaced"], 1);
    let gaps = c["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 2, "{out}");
    assert_eq!(gaps[0]["to"].as_str().unwrap(), out["replay"][0]["read_at"].as_str().unwrap());
    assert_eq!(c["visibility"][0]["source"], "wiki");
    assert_eq!(c["visibility"][0]["fidelity"], "mirrored");
    assert!(c["visibility"][0]["watermark_at"].is_null());
    assert!(c["grants"].is_null());
    // The decision's path crosses the binding's semi-join only where a grant admits.
    assert_eq!(out["decision"]["path"].as_array().unwrap().last().unwrap()["layer"], "default-deny");
}

/// `audit explain --audience <t>` lists by name the roles whose grants read the table,
/// whether `default_grants` do, and per `on_behalf_of` scheme the count of principals and
/// served reads.
// spec: disclosure.explain.audience@4426900d
#[test]
fn the_audience_names_roles_and_counts_principal_classes_without_naming_one() {
    let dir = project(
        NOTES,
        vec![
            read(-600, ADA, &["research/notes"], attr::SERVED),
            read(-500, ADA, &["research/notes"], attr::SERVED),
            read(-400, "user://bo@acme.example", &["research/notes"], attr::SERVED),
            read(-300, "service://nightly", &["research/notes"], attr::SERVED),
            read(-200, "user://eve@acme.example", &["research/notes"], attr::REFUSED),
            read(-100, "user://cy@acme.example", &["hr/salaries"], attr::SERVED),
        ],
    );
    std::fs::create_dir_all(dir.path().join(".contextful/exchange")).unwrap();
    std::fs::write(
        dir.path().join(".contextful/exchange/policy.toml"),
        "expected_iss = \"https://login.acme.example/\"\nrole_claim = \"roles\"\n\n[[role_grants.analyst]]\nactions = [\"read\"]\ntables = [\"research/*\"]\n\n[[role_grants.loader]]\nactions = [\"write\"]\ntables = [\"research/notes\"]\n",
    )
    .unwrap();
    let out = answer(&run(dir.path(), &["audit", "explain", "--project", "research", "--audience", "research/notes"]));
    let a = &out["audience"];
    assert_eq!(a["roles"], json!(["analyst"]), "{out}");
    assert_eq!(a["default_grants"], false);
    assert_eq!(
        a["classes"],
        json!([
            { "class": "service", "principals": 1, "reads": 1 },
            { "class": "user", "principals": 2, "reads": 3 },
        ])
    );
    assert_eq!(out["coverage"]["grants"], ".contextful/exchange/policy.toml");
    let text = out.to_string();
    assert!(!text.contains("ada") && !text.contains("bo@") && !text.contains("nightly"), "{text}");

    // The decision for an analyst admits along the role's grant.
    let out = answer(&run(
        dir.path(),
        &["audit", "explain", "--project", "research", "--table", "research/notes", "--subject", ADA, "--role", "analyst"],
    ));
    assert_eq!(out["decision"]["verdict"], "admit", "{out}");
    assert_eq!(out["decision"]["path"][0], json!({ "layer": "role", "name": "analyst" }));
}

/// A reader-facing explanation rendering the member identities of a group on the path
/// raises `VisibilityIndividualNamed`.
// spec: disclosure.explain.groups-not-members@bc65beeb
#[test]
fn a_role_named_for_a_reader_of_the_table_is_refused_on_the_path_and_in_the_audience() {
    const BO: &str = "user://bo@acme.example";
    let dir = project(
        NOTES,
        vec![read(-600, ADA, &["research/notes"], attr::SERVED), read(-500, BO, &["research/notes"], attr::SERVED)],
    );
    std::fs::create_dir_all(dir.path().join(".contextful/exchange")).unwrap();
    std::fs::write(
        dir.path().join(".contextful/exchange/policy.toml"),
        format!(
            "expected_iss = \"https://login.acme.example/\"\nrole_claim = \"roles\"\n\n[[role_grants.analyst]]\nactions = [\"read\"]\ntables = [\"research/*\"]\n\n[[role_grants.\"{BO}\"]]\nactions = [\"read\"]\ntables = [\"research/notes\"]\n"
        ),
    )
    .unwrap();

    // The audience lists the role named for the reader `bo`.
    let err = refusal(&run(dir.path(), &["audit", "explain", "--project", "research", "--audience", "research/notes"]));
    assert!(err.contains("VisibilityIndividualNamed"), "{err}");
    assert!(!err.contains("bo@"), "{err}");

    // The decision's path names that role.
    let err = refusal(&run(
        dir.path(),
        &["audit", "explain", "--project", "research", "--table", "research/notes", "--subject", ADA, "--role", BO],
    ));
    assert!(err.contains("VisibilityIndividualNamed"), "{err}");

    // A group renders by name alone, and the subject the caller named echoes back.
    let out = answer(&run(
        dir.path(),
        &["audit", "explain", "--project", "research", "--table", "research/notes", "--subject", BO, "--role", "analyst"],
    ));
    assert_eq!(out["decision"]["path"][0], json!({ "layer": "role", "name": "analyst" }));
    assert_eq!(out["subject"], BO);
}
