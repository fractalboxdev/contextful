//! `read.synthesize`: the inference port, the extraction schema and its validation, the
//! attempt budget, and the dead-letter record.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Extract attempts per batch, feedback included (`read.synthesize.extract-attempts`).
pub const EXTRACT_ATTEMPTS: u32 = 3;

/// One chat message sent to the inference endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Message {
        Message { role: role.to_string(), content: content.into() }
    }
}

/// The one inference capability synthesis calls: an operator-configured endpoint
/// answering a conversation with the assistant's content.
pub trait Inference {
    fn complete(&self, messages: &[Message]) -> Result<String, String>;
}

/// A reference from a claim to the landed row it rests on: the row's table and its
/// injected run and sequence, which identify one landed row in every table. On a keyed
/// table the write landing the claim stamps `key`, the cited row's key columns as text,
/// and recall resolves the reference through that key (`read.recall.evidence-key`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub table: String,
    pub run: String,
    pub seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<BTreeMap<String, String>>,
}

impl EvidenceRef {
    /// A reference to one landed row, carrying no key.
    pub fn row(table: impl Into<String>, run: impl Into<String>, seq: i64) -> EvidenceRef {
        EvidenceRef { table: table.into(), run: run.into(), seq, key: None }
    }

    /// The reference's text form inside a prompt.
    pub fn label(&self) -> String {
        format!("{}#{}:{}", self.table, self.run, self.seq)
    }
}

/// A claim as the model proposes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateClaim {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub scope: Option<String>,
    pub confidence: f64,
    pub evidence: Vec<EvidenceRef>,
}

/// An edge as the model proposes it, between two entity mentions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEdge {
    pub rel_type: String,
    pub source: String,
    pub target: String,
}

/// One validated extraction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extraction {
    pub claims: Vec<CandidateClaim>,
    #[serde(default)]
    pub edges: Vec<CandidateEdge>,
}

/// The output schema the extraction prompt states.
pub const OUTPUT_SCHEMA: &str = r#"{"claims": [{"subject": string, "predicate": string, "object": string, "scope": string or null, "confidence": number in [0, 1], "evidence": [{"table": string, "run": string, "seq": integer}]}], "edges": [{"rel_type": string, "source": string, "target": string}]}"#;

/// Validate a response against the output schema, or say why it fails.
pub fn validate(content: &str) -> Result<Extraction, String> {
    let body = content.trim();
    let body = body.strip_prefix("```json").or_else(|| body.strip_prefix("```")).map_or(body, |b| b.trim_end_matches("```").trim());
    let value: Value = serde_json::from_str(body).map_err(|e| format!("the response is not JSON: {e}"))?;
    let extraction: Extraction =
        serde_json::from_value(value).map_err(|e| format!("the response does not match the output schema: {e}"))?;
    for (i, c) in extraction.claims.iter().enumerate() {
        validate_claim(c).map_err(|why| format!("claim {i}: {why}"))?;
    }
    Ok(extraction)
}

/// Validate one claim, whichever path proposes it: non-empty subject, predicate and
/// object, a confidence in [0, 1], and no negative evidence sequence.
pub fn validate_claim(c: &CandidateClaim) -> Result<(), String> {
    for (field, v) in [("subject", &c.subject), ("predicate", &c.predicate), ("object", &c.object)] {
        if v.trim().is_empty() {
            return Err(format!("`{field}` is empty"));
        }
    }
    if !(0.0..=1.0).contains(&c.confidence) {
        return Err(format!("confidence {} is outside [0, 1]", c.confidence));
    }
    if c.evidence.iter().any(|e| e.seq < 0) {
        return Err("an evidence `seq` is negative".into());
    }
    Ok(())
}

/// What one extract attempt yields.
#[derive(Debug, Clone, PartialEq)]
pub enum Attempt {
    Accept(Extraction),
    /// Re-prompt with this feedback.
    Retry(String),
    /// The batch spent its attempts; the last failure's feedback.
    Exhausted(String),
}

/// Judge attempt number `attempt` (from 1) of a batch: a schema-invalid response is
/// re-prompted with its validation feedback, at most 3 attempts in total
/// (`read.synthesize.extract-attempts`).
pub fn judge(attempt: u32, content: &str) -> Attempt {
    match validate(content) {
        Ok(e) => Attempt::Accept(e),
        Err(why) if attempt < EXTRACT_ATTEMPTS => Attempt::Retry(why),
        Err(why) => Attempt::Exhausted(why),
    }
}

/// The message re-prompting a schema-invalid response.
pub fn feedback(why: &str) -> Message {
    Message::new(
        "user",
        format!("The previous response did not validate: {why}. Answer again with JSON alone, matching the output schema: {OUTPUT_SCHEMA}"),
    )
}

/// `sha256:<hex>` over a prompt template.
pub fn template_hash(template: &str) -> String {
    let d = Sha256::digest(template.as_bytes());
    format!("sha256:{}", d.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// One dead-lettered item: the stage, the drop reason's identifier and detail, the
/// response it came from and the template's hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeadLetter {
    pub stage: String,
    pub reason: String,
    pub detail: String,
    pub response: String,
    pub template_hash: String,
}
