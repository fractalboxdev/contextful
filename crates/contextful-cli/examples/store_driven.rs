//! An embedding binary: the `contextful` command line with one compiled row body, `score`,
//! registered before build. Each input row makes one paid call, `model`, and lands one
//! `scores` row carrying the row's `doc_id` and the length of its `body`.
//! With `SCORE_AUDIT` set, each row also lands one `audits` row carrying its `doc_id`.
//! `SCORE_LEDGER` names a file each served call appends `<doc_id> <idempotency key>` to;
//! `SCORE_DIE_AFTER` ends the process inside the call past that many served calls.

use contextful_core::run::drive::{Bodies, Emitted, InputRow, RowBody, RowCalls, RowStop};
use contextful_core::run::{Failure, FailureTag};
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};

struct Score {
    ledger: Option<String>,
    audit: bool,
    die_after: Option<usize>,
    served: Mutex<usize>,
}

impl Score {
    fn serve(&self, doc: &str, key: &str, body: &str) -> Result<Vec<u8>, Failure> {
        let mut served = self.served.lock().unwrap_or_else(|e| e.into_inner());
        if self.die_after.is_some_and(|n| *served >= n) {
            std::process::exit(9);
        }
        if let Some(path) = &self.ledger {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| Failure::new(FailureTag::Transient, e.to_string()))?;
            writeln!(f, "{doc} {key}").map_err(|e| Failure::new(FailureTag::Transient, e.to_string()))?;
        }
        *served += 1;
        Ok(body.chars().count().to_string().into_bytes())
    }
}

impl RowBody for Score {
    fn run(&self, row: &InputRow, calls: &dyn RowCalls) -> Result<Emitted, RowStop> {
        let doc = row.row.get("doc_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let body = row.row.get("body").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let score = calls.call("model", doc.as_bytes(), &mut |key| self.serve(&doc, key, &body))?;
        let score: i64 = String::from_utf8_lossy(&score).parse().unwrap_or_default();
        let row = json!({ "doc_id": doc, "score": score }).as_object().cloned().unwrap_or_default();
        let mut out = Emitted::from([("scores".to_string(), vec![row])]);
        if self.audit {
            out.insert("audits".to_string(), vec![json!({ "doc_id": doc }).as_object().cloned().unwrap_or_default()]);
        }
        Ok(out)
    }
}

fn main() {
    let body = Score {
        ledger: std::env::var("SCORE_LEDGER").ok(),
        audit: std::env::var_os("SCORE_AUDIT").is_some(),
        die_after: std::env::var("SCORE_DIE_AFTER").ok().and_then(|n| n.parse().ok()),
        served: Mutex::new(0),
    };
    let mut bodies = Bodies::default();
    if let Err(e) = bodies.register("score", Arc::new(body)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    contextful_cli::main_host(contextful_cli::Host { bodies, ..contextful_cli::Host::default() });
}
