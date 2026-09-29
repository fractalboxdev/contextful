//! Milestone 5 — the read face under enforcement.
//!
//! Reach: an agent asks over MCP and receives ranked rows the caller's authority admits.

use contextful_acceptance::stdio::Session;
use contextful_acceptance::{bin, GitRepo};
use serde_json::{json, Value};
use std::process::Output;

const AUD: &str = "contextful://acme-research";
const EMAIL: &str = "dana@acme.example";

fn ok(out: &Output) -> String {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Client {
    session: Session,
    next: u64,
}

impl Client {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params }).to_string();
        let answer: Value = serde_json::from_str(&self.session.exchange(&line)).unwrap();
        assert_eq!(answer["id"], json!(self.next), "{answer}");
        answer
    }

    /// Call a tool; `Ok` carries the structured result, `Err` the in-band refusal text.
    fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String> {
        let answer = self.request("tools/call", json!({ "name": tool, "arguments": arguments }));
        let result = &answer["result"];
        assert!(result.is_object(), "a refusal arrives in-band, under transport success: {answer}");
        if result["isError"] == json!(true) {
            Err(result["content"][0]["text"].as_str().unwrap_or_default().to_string())
        } else {
            Ok(result["structuredContent"].clone())
        }
    }

    fn query(&mut self, sql: &str) -> Result<Value, String> {
        self.call("context.query", json!({ "sql": sql }))
    }
}

fn column(result: &Value, name: &str) -> Vec<Value> {
    let columns = result["columns"].as_array().unwrap();
    let i = columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name}: {result}"));
    result["rows"].as_array().unwrap().iter().map(|r| r[i].clone()).collect()
}

#[test]
fn m05_read_face() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 3600\n"));
    p.write(
        "contextful.toml",
        r#"
[[pipeline.tables]]
name = "research/notes"
partition_by = ["tenant"]
agent_description = "Research notes, one partition per tenant."

[pipeline.tables.policy.columns]
author_email = { class = "email", strategy = "hash", combine = "truncate:5" }

[[pipeline.tables]]
name = "research/vendor"

[pipeline.tables.policy.zone]
allow = ["public-cloud:*"]

[[pipeline.tables]]
name = "hr/salaries"
"#,
    );
    p.write(
        "notes.jsonl",
        &[
            json!({"note_id": "n1", "tenant": "acme", "title": "Solar battery storage costs fall", "author_email": EMAIL}),
            json!({"note_id": "n2", "tenant": "acme", "title": "Battery storage for regional grids", "author_email": EMAIL}),
            json!({"note_id": "n3", "tenant": "acme", "title": "Quarterly hiring plan", "author_email": EMAIL}),
            json!({"note_id": "n4", "tenant": "globex", "title": "Solar battery storage at Globex", "author_email": "lee@globex.example"}),
        ]
        .map(|r| r.to_string())
        .join("\n"),
    );
    p.write("vendor.jsonl", &json!({"item_id": "v1", "title": "Solar battery storage feed"}).to_string());
    p.write("salaries.jsonl", &json!({"employee": "e1", "title": "Battery storage engineer salary"}).to_string());
    for (table, rows) in [("research/notes", "notes.jsonl"), ("research/vendor", "vendor.jsonl"), ("hr/salaries", "salaries.jsonl")] {
        ok(&p.run(&cf, &["context", "land", table, "--project", "research", "--rows", rows, "--run-id", "run-0001", "--site-id", "site-a"]));
    }

    // The caller's authority: research tables, the acme tenant of the notes, an on-premises zone.
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let token = ok(&p.run(
        &cf,
        &[
            "token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example",
            "--agent", "agent://research-loop", "--zone", "on-prem:hq", "--action", "read", "--table", "research/*",
            "--tenant", "research/notes=acme", "--ttl", "3600",
        ],
    ));

    let session = Session::spawn(
        &cf,
        &["mcp", "--project", "research", "--public-key", &public, "--audience", AUD],
        &p.root,
        &[("CONTEXTFUL_TOKEN", &token)],
    );
    let mut client = Client { session, next: 0 };

    let init = client.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "acceptance", "version": "0" } }));
    assert_eq!(init["result"]["serverInfo"]["name"], json!("contextful"), "{init}");
    client.session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());

    let tools = client.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for tool in ["context.describe", "context.query", "context.files", "context.file", "corpus.retrieve"] {
        assert!(names.contains(&tool), "{tool} missing from {names:?}");
    }

    // Ranked rows the authority admits: the other tenant's note, the zone-excluded vendor
    // table and the ungranted table all match the question and none appears.
    let ranked = client
        .call("corpus.retrieve", json!({ "prefix": "research/", "query": "solar battery storage", "limit": 10 }))
        .unwrap();
    let rows: Vec<Value> = column(&ranked, "_row");
    let ids: Vec<&str> = rows.iter().map(|r| r["note_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["n1", "n2"], "{ranked}");
    assert!(column(&ranked, "_table").iter().all(|t| t == "research/notes"), "{ranked}");
    let scores: Vec<i64> = column(&ranked, "_score").iter().map(|s| s.as_i64().unwrap()).collect();
    assert_eq!(scores, [3, 2], "{ranked}");
    for row in &rows {
        let masked = row["author_email"].as_str().unwrap();
        assert_eq!(masked.len(), 5, "{row}");
        assert_ne!(masked, EMAIL);
    }
    assert_eq!(ranked["contextful.retrieval"]["returned"], json!(2), "{ranked}");

    // A statement reads through the same restriction.
    let notes = client.query(r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#).unwrap();
    assert_eq!(column(&notes, "note_id"), [json!("n1"), json!("n2"), json!("n3")]);
    // A zone-excluded table answers empty and says so: the restriction block names it and
    // counts what the zone step removed, whatever the statement asked.
    let excluded = json!({
        "zone": "on-prem:hq",
        "incognito": false,
        "tables": [{ "table": "research/vendor", "excluded": true, "rows_dropped": 1, "columns_masked": [] }],
    });
    let vendor = client.query(r#"SELECT * FROM "research/vendor""#).unwrap();
    assert_eq!(vendor["rows"], json!([]), "{vendor}");
    assert_eq!(vendor["contextful.restriction"], excluded, "{vendor}");
    let filtered = client.query(r#"SELECT * FROM "research/vendor" WHERE item_id = 'none'"#).unwrap();
    assert_eq!(filtered["contextful.restriction"], excluded, "{filtered}");
    assert!(notes.get("contextful.restriction").is_none(), "{notes}");
    assert_eq!(ranked["contextful.restriction"], excluded, "{ranked}");
    let described = client.call("context.describe", json!({ "table": "research/vendor" })).unwrap();
    assert_eq!((&described["session_zone"], &described["zone_admitted"]), (&json!("on-prem:hq"), &json!(false)), "{described}");

    // Outside the authority, a read is refused by name rather than answered empty.
    let refused = |r: Result<Value, String>, error: &str| {
        let text = r.expect_err(error);
        assert!(text.contains(error), "expected {error}, got {text}");
    };
    refused(client.query(r#"SELECT * FROM "hr/salaries""#), "EnforceUnknownRelation");
    refused(client.query(r#"SELECT note_id FROM "research/notes" WHERE tenant = 'globex'"#), "EnforceScopeDenied");
    refused(client.query(r#"DELETE FROM "research/notes""#), "StatementNotReadOnly");
    refused(
        client.query("SELECT * FROM read_parquet('.contextful/context/research/tables/hr/salaries/**/*.parquet')"),
        "TableFunctionRefused",
    );

    // A scope declared in one subquery admits nothing of that name in another.
    refused(
        client.query(r#"SELECT * FROM (WITH "hr/salaries" AS (SELECT 1 AS x) SELECT x FROM "hr/salaries") s, "hr/salaries""#),
        "EnforceUnknownRelation",
    );
    refused(client.query(r#"SELECT * FROM (WITH sqlite_master AS (SELECT 1 AS x) SELECT x FROM sqlite_master) s, sqlite_master"#), "TableFunctionRefused");

    // An unsigned zone argument never widens the credential's signed zone.
    refused(
        client.call("corpus.retrieve", json!({ "prefix": "research/vendor", "query": "solar battery storage", "zone": "public-cloud:us-east-1" })),
        "EnforceZoneAssertionWidens",
    );

    assert!(client.session.close().success());
}

/// The operator's raw verb: `contextful query --json` over a project's tables and over
/// local files, printed as the one response projection, with an exact truncation flag.
#[test]
fn m05_operator_query() {
    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write("contextful.toml", "[[pipeline.tables]]\nname = \"research/notes\"\n");
    p.write("notes.jsonl", &[json!({"note_id": "n1"}), json!({"note_id": "n2"}), json!({"note_id": "n3"})].map(|r| r.to_string()).join("\n"));
    ok(&p.run(&cf, &["context", "land", "research/notes", "--project", "research", "--rows", "notes.jsonl", "--run-id", "run-0001", "--site-id", "site-a"]));
    p.write("objects/a.json", "{\"k\":1}\n");
    p.write("objects/b.json", "{\"k\":2}\n");

    let parse = |out: &Output| -> Value { serde_json::from_str(&ok(out)).unwrap() };

    let one = parse(&p.run(&cf, &["query", "--json", "SELECT 1 AS one"]));
    assert_eq!(one, json!({ "columns": ["one"], "rows": [[1]], "truncated": false }));

    let files = format!("SELECT count(*) AS n FROM read_json_objects('{}/objects/*.json')", p.root.display());
    let objects = parse(&p.run(&cf, &["query", "--json", &files]));
    assert_eq!(objects["rows"], json!([["2"]]), "{objects}");

    let sql = r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#;
    let cut = parse(&p.run(&cf, &["query", "--json", "--project", "research", "--limit", "2", sql]));
    assert_eq!(cut, json!({ "columns": ["note_id"], "rows": [["n1"], ["n2"]], "truncated": true }));
    let whole = parse(&p.run(&cf, &["query", "--json", "--project", "research", sql]));
    assert_eq!(whole["rows"], json!([["n1"], ["n2"], ["n3"]]));
    assert_eq!(whole["truncated"], json!(false));

    let two = p.run(&cf, &["query", "--json", "SELECT 1 AS a; SELECT 2 AS b"]);
    assert!(!two.status.success() && two.stdout.is_empty());
    let misspelt = p.run(&cf, &["query", "--json", "--project", "reserch", sql]);
    assert!(!misspelt.status.success() && misspelt.stdout.is_empty());
}

/// A network client's holder key: the credential names its RFC 7638 thumbprint, and each
/// request carries a proof it signs.
struct HolderKey(ed25519_dalek::SigningKey);

impl HolderKey {
    fn x(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0.verifying_key().as_bytes())
    }

    /// The RFC 7638 thumbprint `token mint --holder` takes.
    fn thumbprint(&self) -> String {
        use base64::Engine;
        use sha2::Digest;
        let canonical = format!("{{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"{}\"}}", self.x());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(canonical.as_bytes()))
    }

    /// An EdDSA `dpop+jwt` over `POST /mcp` and `body`, issued now under a fresh nonce.
    fn proof(&self, body: &str) -> String {
        use base64::Engine;
        use ed25519_dalek::Signer;
        use sha2::Digest;
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let header = json!({ "typ": "dpop+jwt", "alg": "EdDSA", "jwk": { "kty": "OKP", "crv": "Ed25519", "x": self.x() } });
        let iat = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let nonce = format!("m05-{}", NONCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let claims = json!({ "htm": "POST", "htu": "/mcp", "bd": b64(&sha2::Sha256::digest(body.as_bytes())), "iat": iat, "jti": nonce });
        let input = format!("{}.{}", b64(header.to_string().as_bytes()), b64(claims.to_string().as_bytes()));
        let signature = self.0.sign(input.as_bytes());
        format!("{input}.{}", b64(&signature.to_bytes()))
    }
}

/// One MCP Streamable HTTP message over a new connection, presenting `token` with a proof
/// from its holder key when one is given; the status, head and body of the answer.
fn post_mcp(addr: &str, message: &Value, token: Option<(&str, &HolderKey)>) -> (u16, String, Vec<u8>) {
    let auth = token.map(|(t, key)| format!("Authorization: DPoP {t}\r\nDPoP: {}\r\n", key.proof(&message.to_string()))).unwrap_or_default();
    post_with(addr, message, &auth)
}

/// One MCP Streamable HTTP message presenting `token` as a bare bearer, with no proof.
fn post_bearer(addr: &str, message: &Value, token: &str) -> (u16, String, Vec<u8>) {
    post_with(addr, message, &format!("Authorization: Bearer {token}\r\n"))
}

fn post_with(addr: &str, message: &Value, auth: &str) -> (u16, String, Vec<u8>) {
    use std::io::{Read, Write};
    let body = message.to_string();
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: {addr}\r\n{auth}Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    (head.split(' ').nth(1).unwrap().parse().unwrap(), head, raw[split + 4..].to_vec())
}

/// The networked read face: one MCP Streamable HTTP listener serves concurrent
/// statements for many credentials — short-lived bearers, and holder-bound credentials
/// each under its own proof — answering what the stdio transport answers for the same grants.
#[test]
fn m05_http_face() {
    use std::io::{BufRead, BufReader};

    let cf = bin("contextful");
    let p = GitRepo::init();
    p.write(".contextful/issuance.toml", &format!("default_audience = \"{AUD}\"\nmax_lifetime_secs = 86400\n"));
    p.write("contextful.toml", "[[pipeline.tables]]\nname = \"research/notes\"\n\n[[pipeline.tables]]\nname = \"research/nums\"\n\n[[pipeline.tables]]\nname = \"hr/salaries\"\n");
    p.write("notes.jsonl", &[json!({"note_id": "n1"}), json!({"note_id": "n2"})].map(|r| r.to_string()).join("\n"));
    p.write("nums.jsonl", &(0..1000).map(|x| json!({ "x": x }).to_string()).collect::<Vec<_>>().join("\n"));
    p.write("salaries.jsonl", &json!({"employee": "e1"}).to_string());
    for (table, rows) in [("research/notes", "notes.jsonl"), ("research/nums", "nums.jsonl"), ("hr/salaries", "salaries.jsonl")] {
        ok(&p.run(&cf, &["context", "land", table, "--project", "research", "--rows", rows, "--run-id", "run-0001", "--site-id", "site-a"]));
    }
    let public = ok(&p.run(&cf, &["token", "keygen", "--out", ".contextful/issuer.seed"]));
    let mint_for = |table: &str, holder: Option<&HolderKey>, ttl: &str| {
        let jkt = holder.map(HolderKey::thumbprint);
        let mut args = vec!["token", "mint", "--issuer-key", ".contextful/issuer.seed", "--on-behalf-of", "user://dana@acme.example", "--zone", "on-prem:hq", "--table", table, "--ttl", ttl];
        if let Some(jkt) = jkt.as_deref() {
            args.extend(["--holder", jkt]);
        }
        ok(&p.run(&cf, &args))
    };
    let mint = |table: &str, holder: Option<&HolderKey>| mint_for(table, holder, "3600");
    let (dana, lee) = (HolderKey(ed25519_dalek::SigningKey::from_bytes(&[5; 32])), HolderKey(ed25519_dalek::SigningKey::from_bytes(&[6; 32])));
    let research_token = mint("research/*", Some(&dana));
    let hr_token = mint("hr/*", Some(&lee));
    let (research, hr) = ((research_token.as_str(), &dana), (hr_token.as_str(), &lee));
    p.write(".contextful/denylist", "");

    /// The listener, killed however the test ends.
    struct Listener(std::process::Child);
    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = std::process::Command::new(&cf)
        .args(["serve", "--http", "127.0.0.1:0", "--audience", AUD, "--max-in-flight", "2", "--project", "research", "--public-key", &public, "--denylist", ".contextful/denylist"])
        .current_dir(&p.root)
        .env_remove("CARGO_TARGET_DIR")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let _listener = Listener(child);
    let addr = loop {
        let mut line = String::new();
        assert!(err.read_line(&mut line).unwrap() > 0, "the face never listened");
        if let Some(a) = line.trim().strip_prefix("listening on http://").and_then(|a| a.strip_suffix("/mcp")) {
            break a.to_string();
        }
    };
    let parse = |b: &[u8]| -> Value { serde_json::from_slice(b).unwrap() };
    let call = |sql: &str| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "context.query", "arguments": { "sql": sql } } });
    let fast = call(r#"SELECT note_id FROM "research/notes" ORDER BY note_id"#);
    let slow = call(r#"SELECT count(*) AS n FROM "research/nums" a, "research/nums" b, "research/nums" c WHERE c.x < 50 AND a.x * b.x = c.x - 7"#);
    let rows = |b: &[u8]| parse(b)["result"]["structuredContent"]["rows"].clone();

    // The handshake reports the network face.
    let (status, _, hello) = post_mcp(&addr, &json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }), Some(research));
    assert_eq!((status, parse(&hello)["result"]["contextful.build"]["faces"].clone()), (200, json!(["http"])));

    // Two credentials on one listener each read their own grants.
    let (status, _, notes) = post_mcp(&addr, &fast, Some(research));
    assert_eq!((status, rows(&notes)), (200, json!([["n1"], ["n2"]])));
    let (status, _, salaries) = post_mcp(&addr, &call(r#"SELECT employee FROM "hr/salaries""#), Some(hr));
    assert_eq!((status, rows(&salaries)), (200, json!([["e1"]])));
    let (_, _, crossed) = post_mcp(&addr, &call(r#"SELECT * FROM "hr/salaries""#), Some(research));
    assert_eq!(parse(&crossed)["result"]["isError"], json!(true));

    // The answer is byte-identical to the stdio answer for the same grants and snapshot;
    // the stdio pipe carries no proof, so its credential binds no key.
    let piped = mint("research/*", None);
    let session = Session::spawn(&cf, &["mcp", "--project", "research", "--public-key", &public, "--audience", AUD], &p.root, &[("CONTEXTFUL_TOKEN", &piped)]);
    let mut client = Client { session, next: 0 };
    let answer = client.request("tools/call", fast["params"].clone());
    assert_eq!(String::from_utf8(notes.clone()).unwrap(), answer.to_string());
    assert!(client.session.close().success());

    // The same credential binding no key, living 3600 s, reads over the network as a bare
    // bearer, byte-identically; one living 3601 s admits nothing.
    let (status, _, bearer_notes) = post_bearer(&addr, &fast, &piped);
    assert_eq!((status, bearer_notes), (200, notes));
    let (status, _, longer) = post_bearer(&addr, &fast, &mint_for("research/*", None, "3601"));
    assert_eq!((status, parse(&longer)["error"]["identifier"].clone()), (401, json!("BearerLifetimeExceeded")));

    // No credential: 401. A holder-bound credential with no proof, or a proof from another key: 401.
    let (status, _, missing) = post_mcp(&addr, &fast, None);
    assert_eq!((status, parse(&missing)["error"]["identifier"].clone()), (401, json!("HttpCredentialMissing")));
    let (status, _, unproven) = post_bearer(&addr, &fast, &research_token);
    assert_eq!((status, parse(&unproven)["error"]["identifier"].clone()), (401, json!("PossessionProofInvalid")));
    let (status, _, stolen) = post_mcp(&addr, &fast, Some((research_token.as_str(), &lee)));
    assert_eq!((status, parse(&stolen)["error"]["identifier"].clone()), (401, json!("PossessionProofInvalid")));

    // A fast statement answers while a slow one runs; past the ceiling of 2, a third answers 503.
    let slow_open = std::sync::atomic::AtomicBool::new(true);
    std::thread::scope(|s| {
        let slow_done = s.spawn(|| {
            assert_eq!(post_mcp(&addr, &slow, Some(research)).0, 200);
            slow_open.store(false, std::sync::atomic::Ordering::SeqCst);
            std::time::Instant::now()
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(slow_open.load(std::sync::atomic::Ordering::SeqCst), "the slow statement is in flight");
        assert_eq!(post_mcp(&addr, &fast, Some(research)).0, 200);
        let fast_answered = std::time::Instant::now();
        // The fast answer arrives while the slow request is still open.
        assert!(slow_open.load(std::sync::atomic::Ordering::SeqCst), "the fast statement answered only after the slow one completed");
        let second_slow = s.spawn(|| post_mcp(&addr, &slow, Some(research)).0);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (status, head, _) = post_mcp(&addr, &fast, Some(research));
        assert_eq!(status, 503, "{head}");
        assert!(head.contains("Retry-After: 1"), "{head}");
        let margin = slow_done.join().unwrap().checked_duration_since(fast_answered);
        assert!(margin.is_some_and(|m| m >= std::time::Duration::from_millis(100)), "the slow statement completed {margin:?} after the fast answer");
        assert_eq!(second_slow.join().unwrap(), 200);
    });

    // Revoked between requests: refused on the next, with no restart.
    let introspected: Value = serde_json::from_str(&ok(&p.run(&cf, &["token", "introspect", "--token", &research_token]))).unwrap();
    p.write(".contextful/denylist", &format!("{}\n", introspected["rev_id"].as_str().unwrap()));
    let (status, _, revoked) = post_mcp(&addr, &fast, Some(research));
    assert_eq!((status, parse(&revoked)["error"]["identifier"].clone()), (401, json!("AuthorityRevoked")));
    assert_eq!(post_mcp(&addr, &call(r#"SELECT employee FROM "hr/salaries""#), Some(hr)).0, 200);
}
