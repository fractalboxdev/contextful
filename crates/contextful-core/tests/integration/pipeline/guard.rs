//! `run.guard-secrets`: the mask over credential-shaped spans.

use contextful_core::pipeline::guard::{guard_rows, mask, spans, Kind, MARKER};
use contextful_core::run::ports::Row;
use serde_json::json;

const PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA7\n-----END RSA PRIVATE KEY-----";

/// Matchers are linear-time, regex-free forward scans, each anchored on a literal prefix; the credential
/// catalogue lives in code under a precision and recall fixture test.
// spec: run.guard-secrets.matchers@f276346d
#[test]
fn the_catalogue_holds_its_precision_and_recall_fixture() {
    let positives = [
        ("id AKIAIOSFODNN7EXAMPLE here", Kind::AwsAccessKeyId),
        ("ASIAY34FZKBOKMUTVV7A", Kind::AwsAccessKeyId),
        (PEM, Kind::PemPrivateKey),
        ("ghp_0123456789abcdefghijABCDEFGHIJ012345", Kind::GithubToken),
        ("github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz", Kind::GithubToken),
        ("xoxb-123456789012-abcdefABCDEF", Kind::SlackToken),
        ("password=hunter2hunter2", Kind::Assignment),
        ("api_key: 'zK9s8d7f6g5h'", Kind::Assignment),
        ("OPENAI_API_KEY sk-proj-Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4z_AbCdEfGh", Kind::LlmProviderKey),
        ("key sk-ant-api03-Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAbCdEfGh-AA", Kind::LlmProviderKey),
        ("sk-Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAbCd", Kind::LlmProviderKey),
        ("maps AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q here", Kind::GoogleApiKey),
        ("sk_live_4eC39HqLyjWDarjtT1zdp7dc", Kind::StripeKey),
        ("rk_live_51H8xYzAbCdEfGhIjKlMnOpQr", Kind::StripeKey),
        (
            "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            Kind::Jwt,
        ),
        ("Authorization: Bearer 9f8e7d6c5b4a39218a7b6c5d4e3f2a1b", Kind::Bearer),
    ];
    for (text, kind) in positives {
        let found = spans(text);
        assert_eq!(found.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [kind], "{text}");
    }
    let negatives = [
        "AKIAIOSFODNN7EXAMPL",                    // 15 characters after the prefix
        "XAKIAIOSFODNN7EXAMPLEX",                 // embedded in a longer word
        "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----",
        "ghp_short",
        "xoxb-12",
        "password=short",                         // below the token length
        "tokenizer=whitespace_standard",          // keyword inside a longer word
        "the password is set elsewhere",
        "order 20300101 shipped to AKIA street",
        "a risk-free task-list",
        "import sk-learn as the baseline",
        "my-sk-learn-pipeline-config-v2-with-a-long-tail",
        "desk-1234567890abcdefghijkl",
        "release 1.2.3",
        "xeyJabc.def.ghi",
        "AIzaTooShort",
        "sk_live_short",
        "Bearer short",
        "Authorization: Basic",
    ];
    for text in negatives {
        assert!(spans(text).is_empty(), "false positive on {text:?}: {:?}", spans(text));
    }
    // One forward scan per pattern: a long adversarial input stays linear.
    let long = "AKIA".repeat(250_000) + &"password=".repeat(100_000) + &"sk-".repeat(250_000) + &"eyJ.".repeat(250_000) + &"bearer ".repeat(100_000);
    let started = std::time::Instant::now();
    let _ = spans(&long);
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}

/// The replacement covers only the matched byte ranges, widened to character boundaries; overlapping spans merge
/// under the higher-priority pattern.
// spec: run.guard-secrets.mask-span@0e3495ed
#[test]
fn only_the_matched_range_is_replaced_and_overlaps_merge() {
    assert_eq!(mask("before AKIAIOSFODNN7EXAMPLE after").unwrap(), format!("before {MARKER} after"));
    // A multi-byte value ends on a character boundary.
    assert_eq!(mask("secret=ÄbcdefghijÖ tail").unwrap(), format!("secret={MARKER} tail"));
    // An assignment whose value is an AWS key: one span, under the AWS pattern.
    let found = spans("token=AKIAIOSFODNN7EXAMPLE");
    assert_eq!(found, [(Kind::AwsAccessKeyId, 6..26)]);
    assert_eq!(mask("token=AKIAIOSFODNN7EXAMPLE").unwrap(), format!("token={MARKER}"));
    // A bearer header carrying a JWT: one span, under the JWT pattern, the scheme kept.
    let header = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlLXNpZ25hdHVyZQ";
    assert_eq!(spans(header).into_iter().map(|(k, _)| k).collect::<Vec<_>>(), [Kind::Jwt]);
    assert_eq!(mask(header).unwrap(), format!("Authorization: Bearer {MARKER}"));
    assert_eq!(mask("nothing to see"), None);
}

/// The replacement is the fixed `[REDACTED:secret]` marker, never a shape-preserving transform; an assignment
/// keeps its `key=` prefix.
// spec: run.guard-secrets.mask-replacement@26befa44
#[test]
fn the_marker_is_fixed_and_an_assignment_keeps_its_key() {
    assert_eq!(MARKER, "[REDACTED:secret]");
    for secret in ["AKIAIOSFODNN7EXAMPLE", "ghp_0123456789abcdefghijABCDEFGHIJ012345", PEM] {
        assert_eq!(mask(secret).unwrap(), MARKER, "no shape of {secret} survives");
    }
    assert_eq!(mask("password=hunter2hunter2&next=1").unwrap(), format!("password={MARKER}&next=1"));
    assert_eq!(mask("API_KEY : zK9s8d7f6g5h").unwrap(), format!("API_KEY : {MARKER}"));
}

/// The guard is on by default and blocks no run; each pull logs the count of masked cells per column.
#[test]
fn the_guard_counts_masked_cells_per_column_and_blocks_nothing() {
    let mut batch: Vec<Row> = [json!({"note": "AKIAIOSFODNN7EXAMPLE", "id": 1}), json!({"note": "fine", "id": 2}), json!({"note": "password=hunter2hunter2"})]
        .into_iter()
        .map(|v| v.as_object().unwrap().clone())
        .collect();
    let counts = guard_rows(&mut batch);
    assert_eq!(counts.into_iter().collect::<Vec<_>>(), [("note".to_string(), 2)]);
    assert_eq!(batch.len(), 3, "every row stays");
    assert_eq!(batch[1]["note"], "fine");
}

/// The guard reads pre-normalize string cells for plaintext shapes; encoded material and a credential split
/// across two cells pass through.
// spec: run.guard-secrets.coverage@39bb86ef
#[test]
fn encoded_or_split_credentials_pass_through() {
    let mut batch: Vec<Row> = [json!({
        // base64 of an AWS key id
        "encoded": "QUtJQUlPU0ZPRE5ON0VYQU1QTEU=",
        "left": "AKIAIOSFOD",
        "right": "NN7EXAMPLE",
        "nested": {"deep": ["AKIAIOSFODNN7EXAMPLE"]},
        "count": 7
    })]
    .into_iter()
    .map(|v| v.as_object().unwrap().clone())
    .collect();
    let counts = guard_rows(&mut batch);
    assert_eq!(batch[0]["encoded"], "QUtJQUlPU0ZPRE5ON0VYQU1QTEU=");
    assert_eq!((batch[0]["left"].as_str(), batch[0]["right"].as_str()), (Some("AKIAIOSFOD"), Some("NN7EXAMPLE")));
    // A nested string is still a pre-normalize string cell.
    assert_eq!(batch[0]["nested"]["deep"][0], MARKER);
    assert_eq!(counts.keys().collect::<Vec<_>>(), ["nested"]);
}
