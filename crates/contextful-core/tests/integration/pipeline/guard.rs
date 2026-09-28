//! `run.guard-secrets`: the mask over credential-shaped spans.

use contextful_core::pipeline::guard::{guard_rows, mask, spans, Kind, KEYWORDS, MARKER};
use contextful_core::run::ports::Row;
use serde_json::json;

const PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA7\n-----END RSA PRIVATE KEY-----";

/// Every credential kind, in the order its recall reports.
const KINDS: [Kind; 10] = [
    Kind::PemPrivateKey,
    Kind::LlmProviderKey,
    Kind::GoogleApiKey,
    Kind::StripeKey,
    Kind::Jwt,
    Kind::AwsAccessKeyId,
    Kind::GithubToken,
    Kind::SlackToken,
    Kind::Bearer,
    Kind::Assignment,
];

/// A kind's place in [`KINDS`]. The match is exhaustive, so a new kind fails to compile
/// until the corpus generates it.
fn slot(k: Kind) -> usize {
    match k {
        Kind::PemPrivateKey => 0,
        Kind::LlmProviderKey => 1,
        Kind::GoogleApiKey => 2,
        Kind::StripeKey => 3,
        Kind::Jwt => 4,
        Kind::AwsAccessKeyId => 5,
        Kind::GithubToken => 6,
        Kind::SlackToken => 7,
        Kind::Bearer => 8,
        Kind::Assignment => 9,
    }
}

/// Stripe secret and restricted key prefixes, live and test mode.
const STRIPE: [&str; 4] = ["sk_live_", "rk_live_", "sk_test_", "rk_test_"];

/// Seed of the labelled catalogue corpus.
const CATALOGUE_SEED: u64 = 0x5eed_0003;
/// Generated positives, and generated near-misses, per kind.
const PER_KIND: usize = 64;

const UPPER_DIGIT: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const BASE64URL: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const BASE64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const TOKEN68: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
/// Assignment values: no delimiter the matcher stops at, and no `-` or `_` opening another shape.
const VALUE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%*+/";
/// Heads a key carries before its keyword: none, or a word joined by `_`, `-` or `.`.
const COMPOUND_PREFIXES: [&str; 6] = ["", "", "client_", "x-", "app.", "OAUTH_"];
/// Words around a sample; none is a keyword or opens a credential shape.
const CONTEXT: [&str; 8] = ["id", "row", "note", "ref", "for", "the", "cell", "sent"];

/// A linear congruential sequence: every sample of the corpus derives from its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn within(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }

    /// Between `lo` and `hi` characters of `alphabet`.
    fn text(&mut self, alphabet: &str, lo: usize, hi: usize) -> String {
        let len = self.within(lo, hi);
        self.chars(alphabet, len)
    }

    fn chars(&mut self, alphabet: &str, len: usize) -> String {
        let a = alphabet.as_bytes();
        (0..len).map(|_| a[self.below(a.len())] as char).collect()
    }
}

/// A positive of `kind`: the sample and the byte range the mask must replace.
fn positive(r: &mut Lcg, kind: Kind) -> (String, std::ops::Range<usize>) {
    let whole = |s: String| {
        let n = s.len();
        (s, 0..n)
    };
    match kind {
        Kind::PemPrivateKey => {
            let label = r.pick(&["RSA ", "EC ", "OPENSSH ", "DSA ", "ENCRYPTED ", ""]);
            let count = r.within(1, 3);
            let lines: Vec<String> = (0..count).map(|_| r.chars(BASE64, 64)).collect();
            whole(format!("-----BEGIN {label}PRIVATE KEY-----\n{}\n-----END {label}PRIVATE KEY-----", lines.join("\n")))
        }
        Kind::LlmProviderKey => match r.below(3) {
            0 => whole(format!("sk-{}", r.chars(ALNUM, 48))),
            1 => whole(format!("sk-proj-{}", r.text(BASE64URL, 40, 120))),
            _ => whole(format!("sk-ant-api03-{}-AA", r.text(BASE64URL, 80, 95))),
        },
        Kind::GoogleApiKey => whole(format!("AIza{}", r.chars(BASE64URL, 35))),
        Kind::StripeKey => whole(format!("{}{}", r.pick(&STRIPE), r.text(ALNUM, 24, 99))),
        Kind::Jwt => whole(format!(
            "eyJ{}.eyJ{}.{}",
            r.text(BASE64URL, 10, 40),
            r.text(BASE64URL, 10, 80),
            r.text(BASE64URL, 20, 86)
        )),
        Kind::AwsAccessKeyId => whole(format!("{}{}", r.pick(&["AKIA", "ASIA"]), r.chars(UPPER_DIGIT, 16))),
        Kind::GithubToken => match r.below(2) {
            0 => whole(format!("{}{}", r.pick(&["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]), r.chars(ALNUM, 36))),
            _ => whole(format!("github_pat_{}_{}", r.chars(ALNUM, 22), r.chars(ALNUM, 59))),
        },
        Kind::SlackToken => whole(format!(
            "{}{}-{}-{}",
            r.pick(&["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"]),
            r.chars("0123456789", 12),
            r.chars("0123456789", 12),
            r.chars(ALNUM, 24)
        )),
        Kind::Bearer => {
            let head = format!("Authorization: {} ", r.pick(&["Bearer", "bearer", "BEARER"]));
            let token = r.text(TOKEN68, 20, 64) + &"=".repeat(r.below(3));
            let span = head.len()..head.len() + token.len();
            (head + &token, span)
        }
        Kind::Assignment => {
            let k = r.pick(&KEYWORDS);
            let key = match r.below(3) {
                0 => k.to_string(),
                1 => k.to_ascii_uppercase(),
                _ => k[..1].to_ascii_uppercase() + &k[1..],
            };
            let key = format!("{}{key}", r.pick(&COMPOUND_PREFIXES));
            let (open, close) = [("=", ""), (": ", ""), (" = ", ""), ("=\"", "\""), (": '", "'")][r.below(5)];
            let value = r.text(VALUE, 8, 40);
            let head = format!("{key}{open}");
            let span = head.len()..head.len() + value.len();
            (format!("{head}{value}{close}"), span)
        }
    }
}

/// A near-miss of `kind`: one step short of its shape, or its shape inside a longer word.
fn near_miss(r: &mut Lcg, kind: Kind) -> String {
    match kind {
        Kind::PemPrivateKey => {
            let label = r.pick(&["PUBLIC KEY", "CERTIFICATE", "RSA PUBLIC KEY", "CERTIFICATE REQUEST"]);
            format!("-----BEGIN {label}-----\n{}\n-----END {label}-----", r.chars(BASE64, 64))
        }
        Kind::LlmProviderKey => match r.below(2) {
            0 => format!("sk-{}", r.text(BASE64URL, 1, 31)),
            _ => format!("{}sk-{}", r.pick(&["task-", "desk-", "risk-", "ask-"]), r.chars(ALNUM, 40)),
        },
        Kind::GoogleApiKey => match r.below(2) {
            0 => {
                let len = if r.below(2) == 0 { 34 } else { 36 };
                format!("AIza{}", r.chars(BASE64URL, len))
            }
            _ => format!("x{}AIza{}", r.chars(ALNUM, 3), r.chars(BASE64URL, 35)),
        },
        Kind::StripeKey => format!("{}{}", r.pick(&STRIPE), r.text(ALNUM, 1, 23)),
        Kind::Jwt => match r.below(2) {
            0 => format!("eyJ{}.eyJ{}", r.chars(ALNUM, 12), r.chars(ALNUM, 20)),
            _ => format!("eyJ{}.{}.{}", r.chars(ALNUM, 12), r.chars(ALNUM, 20), r.chars(ALNUM, 20)),
        },
        Kind::AwsAccessKeyId => match r.below(3) {
            0 => format!("{}{}", r.pick(&["AKIA", "ASIA"]), r.chars(UPPER_DIGIT, 15)),
            1 => format!("{}{}", r.pick(&["AKIA", "ASIA"]), r.chars(UPPER_DIGIT, 17)),
            _ => format!("X{}{}", r.pick(&["AKIA", "ASIA"]), r.chars(UPPER_DIGIT, 16)),
        },
        Kind::GithubToken => match r.below(2) {
            0 => format!("{}{}", r.pick(&["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]), r.text(ALNUM, 1, 35)),
            _ => format!("github_pat_{}", r.text(ALNUM, 1, 21)),
        },
        Kind::SlackToken => format!("{}{}", r.pick(&["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"]), r.text(ALNUM, 1, 9)),
        Kind::Bearer => match r.below(2) {
            0 => format!("Authorization: Bearer {}", r.text(TOKEN68, 1, 19)),
            _ => format!("unbearer {}", r.chars(ALNUM, 32)),
        },
        Kind::Assignment => {
            let k = r.pick(&KEYWORDS);
            match r.below(3) {
                0 => format!("{k}={}", r.text(VALUE, 1, 7)),
                1 => format!("{}{k}={}", r.pick(&["session", "my", "x", "o"]), r.chars(VALUE, 16)),
                _ => format!("the {k} is {}", r.chars(ALNUM, 16)),
            }
        }
    }
}

/// Matchers are linear-time, regex-free forward scans, each anchored on a literal prefix; the credential
/// catalogue lives in code under a precision and recall fixture test.
// spec: run.guard-secrets.matchers@f276346d
#[test]
fn the_catalogue_holds_its_precision_and_recall_fixture() {
    assert!(KINDS.iter().enumerate().all(|(i, k)| slot(*k) == i), "KINDS lists every kind once, in slot order");
    let mut r = Lcg(CATALOGUE_SEED);
    let context = |r: &mut Lcg| (r.pick(&CONTEXT), r.pick(&CONTEXT));

    // Positives: a hit is exactly one span, of the labelled kind, over exactly the secret.
    let (mut hits, mut totals) = ([0u64; KINDS.len()], [0u64; KINDS.len()]);
    for kind in KINDS {
        for _ in 0..PER_KIND {
            let (secret, span) = positive(&mut r, kind);
            let (before, after) = context(&mut r);
            let text = format!("{before} {secret} {after}");
            let at = before.len() + 1;
            let expected = [(kind, at + span.start..at + span.end)];
            totals[slot(kind)] += 1;
            if spans(&text) == expected {
                hits[slot(kind)] += 1;
            } else {
                eprintln!("{kind:?} missed {text:?}: {:?}", spans(&text));
            }
        }
    }

    // Negatives: generated near-misses of every kind, and hand-written traps.
    let mut negatives: Vec<String> = Vec::new();
    for kind in KINDS {
        for _ in 0..PER_KIND {
            let miss = near_miss(&mut r, kind);
            let (before, after) = context(&mut r);
            negatives.push(format!("{before} {miss} {after}"));
        }
    }
    negatives.extend(
        [
            "tokenizer=whitespace_standard",
            "the password is set elsewhere",
            "order 20300101 shipped to AKIA street",
            "a risk-free task-list",
            "import sk-learn as the baseline",
            "my-sk-learn-pipeline-config-v2-with-a-long-tail",
            "release 1.2.3",
            "task_test_4eC39HqLyjWDarjtT1zdp7dc",
            "disk_test_suite_runs_nightly_on_the_ci_box",
            "xeyJabc.def.ghi",
            "Authorization: Basic",
        ]
        .map(str::to_string),
    );
    let false_positives: Vec<&String> = negatives.iter().filter(|t| !spans(t).is_empty()).collect();
    false_positives.iter().for_each(|t| eprintln!("false positive on {t:?}: {:?}", spans(t)));

    let recall: Vec<f64> = KINDS.iter().map(|k| hits[slot(*k)] as f64 / totals[slot(*k)].max(1) as f64).collect();
    KINDS.iter().zip(&recall).for_each(|(k, r)| eprintln!("recall {k:?}: {r}"));
    let min_recall = recall.iter().copied().fold(1.0, f64::min);
    let positives: u64 = totals.iter().sum();
    crate::emit("guard-catalogue", min_recall, positives, CATALOGUE_SEED);
    crate::emit("guard-false-positives", false_positives.len() as f64, negatives.len() as u64, CATALOGUE_SEED);
    assert_eq!(min_recall, 1.0, "every kind masks each of its {PER_KIND} positives exactly");
    assert!(false_positives.is_empty(), "{} of {} negatives matched", false_positives.len(), negatives.len());

    // One forward scan per pattern: a long adversarial input stays linear.
    let long = "AKIA".repeat(250_000) + &"password=".repeat(100_000) + &"sk-".repeat(250_000) + &"sk_test_".repeat(250_000) + &"eyJ.".repeat(250_000) + &"bearer ".repeat(100_000);
    let started = std::time::Instant::now();
    let _ = spans(&long);
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}

/// Seed of the short-assignment table's value generator.
const SHORT_ASSIGNMENT_SEED: u64 = 0x5eed_0002;

/// A value of `len` characters from `[a-z0-9]`, drawn from a linear congruential sequence.
fn value(state: &mut u64, len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..len)
        .map(|_| {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ALPHABET[((*state >> 33) % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

#[test]
fn a_keyword_assignment_is_masked_from_8_chars_keeping_its_key() {
    let mut state = SHORT_ASSIGNMENT_SEED;
    let (mut masked, mut total) = (0u64, 0u64);
    for keyword in KEYWORDS {
        for len in 1..=15 {
            let v = value(&mut state, len);
            for (text, kept) in [
                (format!("{keyword}={v}"), format!("{keyword}={MARKER}")),
                (format!("{keyword}: {v}"), format!("{keyword}: {MARKER}")),
                (format!("{keyword}=\"{v}\""), format!("{keyword}=\"{MARKER}\"")),
            ] {
                let out = mask(&text);
                if len >= 8 {
                    total += 1;
                    masked += u64::from(out.as_deref() == Some(kept.as_str()));
                } else {
                    assert_eq!(out, None, "{text}: a value under 8 chars stays");
                }
            }
        }
    }
    // A keyword glued to a letter is no keyword, so its assignment stays.
    assert_eq!(mask("sessiontoken=abcdefgh12"), None);
    let rate = masked as f64 / total as f64;
    crate::emit("guard-short-assignment", rate, total, SHORT_ASSIGNMENT_SEED);
    assert_eq!(masked, total, "{masked} of {total} short assignments masked with their key kept");
}

/// Seed of the compound-key slice's value generator.
const COMPOUND_KEYS_SEED: u64 = 0x5eed_0004;

/// Compound keys: a keyword closing a key after `_`, `-` or `.`, with `-` read as `_`.
const COMPOUND_KEYS: [&str; 12] = [
    "client_secret",
    "access_token",
    "refresh_token",
    "api-key",
    "x-api-key",
    "X-API-Key",
    "CLIENT_SECRET",
    "db.password",
    "aws_secret_access_key",
    "slack-token",
    "access-key",
    "proxy_auth",
];

/// An assignment key qualifies when it is a keyword or ends in one after `_`, `-` or `.`, reading `-` as `_`, so
/// `client_secret`, `x-api-key` and `db.password` qualify and `clientsecret` does not.
// spec: run.guard-secrets.assignment-key@f4f3e5d7
#[test]
fn a_compound_key_assignment_is_masked_keeping_its_key() {
    let mut state = COMPOUND_KEYS_SEED;
    let (mut masked, mut total) = (0u64, 0u64);
    for key in COMPOUND_KEYS {
        for len in [8, 16, 40] {
            let v = value(&mut state, len);
            for (text, kept) in [
                (format!("{key}={v}"), format!("{key}={MARKER}")),
                (format!("{key}: {v}"), format!("{key}: {MARKER}")),
                (format!("--{key} = '{v}'"), format!("--{key} = '{MARKER}'")),
            ] {
                total += 1;
                let out = mask(&text);
                if out.as_deref() == Some(kept.as_str()) {
                    masked += 1;
                } else {
                    eprintln!("{text:?} -> {out:?}");
                }
            }
        }
    }
    let rate = masked as f64 / total as f64;
    crate::emit("guard-compound-keys", rate, total, COMPOUND_KEYS_SEED);
    assert_eq!(masked, total, "{masked} of {total} compound-key assignments masked with their key kept");
    // A keyword glued to a letter or digit, or running on into more key, stays part of a longer word.
    for miss in ["clientsecret=abcdefgh12", "secret2=abcdefgh12", "api-keys=abcdefgh12", "token_type=abcdefgh12", "tokenizer=whitespace_standard"] {
        assert_eq!(mask(miss), None, "{miss}");
    }
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
