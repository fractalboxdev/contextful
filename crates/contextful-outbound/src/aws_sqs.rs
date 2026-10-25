//! The SQS adapter behind the `Queue` port, behind the `aws-sqs` feature
//! (`topology.compose.queue-sqs`): the JSON protocol over the mediated client, each request
//! signed with Signature Version 4 under the workload role. A receive long-polls once, then
//! drains what already waits (`topology.compose.queue-sqs-wait`); an acknowledgement deletes
//! the message (`topology.compose.queue-sqs-visibility`); an S3 event notification on the
//! queue becomes object-arrival keys (`topology.compose.queue-object-notification`).

use crate::client::{Client, HeaderValue};
use crate::role::RoleProvider;
use crate::sigv4::{sign, SigningKey};
use contextful_core::connector::attach::{scrub, Allowlist};
use contextful_core::connector::reference::Hydrated;
use contextful_core::ports::Clock;
use contextful_core::run::queue::{Delivery, Queue, QueueMessage};
use contextful_core::run::{Failure, FailureTag};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use url::Url;

/// The variable naming the queue; unset, the local spool serves.
pub const QUEUE_URL: &str = "CONTEXTFUL_QUEUE_URL";
/// Overrides the regional SQS endpoint.
pub const ENDPOINT: &str = "AWS_ENDPOINT_URL_SQS";
/// The long poll of a receive's first call, in seconds (`topology.compose.queue-sqs-wait`).
pub const WAIT_SECS: u64 = 20;
/// The most messages one receive call asks for (`topology.compose.queue-sqs-batch`).
pub const BATCH: usize = 10;
/// How long a received message stays hidden, in seconds
/// (`topology.compose.queue-sqs-visibility`).
pub const VISIBILITY_SECS: u64 = 300;
/// The largest message SQS carries, in bytes (`topology.compose.queue-sqs-cap`).
pub const MESSAGE_CAP: usize = 1 << 20;
/// The error type SQS answers for a receipt handle that no longer names a message.
const STALE_RECEIPTS: [&str; 2] = ["ReceiptHandleIsInvalid", "InvalidParameterValue"];

fn config(why: String) -> Failure {
    Failure::deterministic(FailureTag::Config, why)
}

/// The SQS adapter over the workload role.
pub struct AwsSqs {
    queue_url: String,
    endpoint: Url,
    region: String,
    role: Arc<RoleProvider>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl AwsSqs {
    /// The adapter `vars` configure, or `None` when [`QUEUE_URL`] is unset: the region from
    /// `AWS_REGION` or `AWS_DEFAULT_REGION`, and the regional endpoint unless [`ENDPOINT`]
    /// names one.
    pub fn from_vars(vars: &BTreeMap<String, String>, clock: Arc<dyn Clock + Send + Sync>) -> Result<Option<AwsSqs>, Failure> {
        let var = |k: &str| vars.get(k).map(|v| v.trim()).filter(|v| !v.is_empty()).map(str::to_string);
        let Some(queue_url) = var(QUEUE_URL) else { return Ok(None) };
        let region = var("AWS_REGION")
            .or_else(|| var("AWS_DEFAULT_REGION"))
            .ok_or_else(|| config(format!("`{QUEUE_URL}` names a queue and neither `AWS_REGION` nor `AWS_DEFAULT_REGION` is set")))?;
        let base = var(ENDPOINT).unwrap_or_else(|| format!("https://sqs.{region}.amazonaws.com"));
        let endpoint = Url::parse(&format!("{}/", base.trim_end_matches('/'))).map_err(|e| config(format!("`{ENDPOINT}` is not a URL: {e}")))?;
        let role = Arc::new(RoleProvider::new(vars.clone()).with_clock(clock.clone()));
        Ok(Some(AwsSqs { queue_url, endpoint, region, role, clock }))
    }

    /// The adapter signing under `role`, the process's one workload-role provider.
    pub fn with_role(mut self, role: Arc<RoleProvider>) -> AwsSqs {
        self.role = role;
        self
    }

    /// One signed call of `action` with `body`: the answer, or `Err` carrying the error type
    /// SQS named alongside the failure.
    fn call(&self, action: &str, body: &Value) -> Result<Result<Value, (String, Failure)>, Failure> {
        let credential = self.role.credentials().map_err(|e| Failure::new(FailureTag::Transient, format!("the SQS queue signs under the workload role: {e}")))?;
        let payload = body.to_string();
        let host = match self.endpoint.port() {
            Some(port) => format!("{}:{port}", self.endpoint.host_str().unwrap_or_default()),
            None => self.endpoint.host_str().unwrap_or_default().to_string(),
        };
        let target = format!("AmazonSQS.{action}");
        let plain = [("content-type", "application/x-amz-json-1.0"), ("host", host.as_str()), ("x-amz-target", target.as_str())];
        let key = SigningKey {
            access_key_id: credential.access_key_id.reveal(),
            secret_access_key: credential.secret_access_key.reveal(),
            session_token: credential.session_token.as_ref().map(Hydrated::reveal),
        };
        let signed = sign("POST", self.endpoint.path(), "", &plain, payload.as_bytes(), &key, &self.region, "sqs", self.clock.now());
        let mut headers: Vec<(String, HeaderValue)> =
            plain.iter().filter(|(k, _)| *k != "host").map(|(k, v)| (k.to_string(), HeaderValue::Plain(v.to_string()))).collect();
        for (k, v) in signed {
            let value = if k == "x-amz-date" { HeaderValue::Plain(v) } else { HeaderValue::Sensitive(Hydrated::new(v).into()) };
            headers.push((k, value));
        }
        let allow = Allowlist::parse(&[self.endpoint.host_str().unwrap_or_default()]).map_err(|e| config(e.to_string()))?;
        let resp = Client::new(allow, self.endpoint.clone()).send_once("POST", &self.endpoint, &headers, Some(payload.as_bytes()))?;
        let answer: Value = serde_json::from_slice(&resp.body).unwrap_or_default();
        if resp.status == 200 {
            return Ok(Ok(answer));
        }
        let kind = answer.get("__type").and_then(Value::as_str).unwrap_or_default().rsplit('#').next().unwrap_or_default().to_string();
        let what = format!("`{}` {action}", scrub(&self.endpoint));
        let failure = match resp.status {
            429 | 500.. => Failure::new(FailureTag::Transient, format!("{what} answered {} {kind}", resp.status)),
            _ => Failure::deterministic(FailureTag::Permanent, format!("{what} answered {} {kind}", resp.status)),
        };
        Ok(Err((kind, failure)))
    }

    fn answered(&self, action: &str, body: &Value) -> Result<Value, Failure> {
        self.call(action, body)?.map_err(|(_, f)| f)
    }
}

/// The messages one SQS message body carries: a port message as the route queued it, or the
/// object-arrival keys of an S3 event notification's `ObjectCreated` records. `None` for a
/// body neither reads as, which stays queued until the queue's redrive policy moves it.
pub fn parse_body(body: &str) -> Option<Vec<QueueMessage>> {
    let value: Value = serde_json::from_str(body).ok()?;
    if value.get("kind").is_some() {
        return serde_json::from_value(value).ok().map(|m| vec![m]);
    }
    if value.get("Event").and_then(Value::as_str) == Some("s3:TestEvent") {
        return Some(Vec::new());
    }
    let records = value.get("Records")?.as_array()?;
    Some(
        records
            .iter()
            .filter(|r| r.get("eventSource").and_then(Value::as_str) == Some("aws:s3"))
            .filter(|r| r.get("eventName").and_then(Value::as_str).is_some_and(|n| n.starts_with("ObjectCreated:")))
            .filter_map(|r| {
                let bucket = r.pointer("/s3/bucket/name")?.as_str()?;
                let key = form_decode(r.pointer("/s3/object/key")?.as_str()?)?;
                Some(QueueMessage::ObjectArrival { bucket: bucket.to_string(), key })
            })
            .collect(),
    )
}

/// An S3 notification's object key decoded: `+` is a space and `%XX` a byte.
fn form_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

impl Queue for AwsSqs {
    fn send(&self, message: &QueueMessage) -> Result<(), Failure> {
        let body = serde_json::to_string(message).map_err(|e| config(format!("encoding a queue message: {e}")))?;
        self.answered("SendMessage", &json!({ "QueueUrl": self.queue_url, "MessageBody": body }))?;
        Ok(())
    }

    /// Up to `max` deliveries: the first call long-polls, each later call waits on none, and
    /// the first empty answer ends the receive. A message holding no delivery is deleted.
    fn receive(&self, max: usize) -> Result<Vec<Delivery>, Failure> {
        let mut deliveries = Vec::new();
        let mut first = true;
        while deliveries.len() < max {
            let ask = (max - deliveries.len()).min(BATCH);
            let wait = if first { WAIT_SECS } else { 0 };
            first = false;
            let answer = self.answered(
                "ReceiveMessage",
                &json!({ "QueueUrl": self.queue_url, "MaxNumberOfMessages": ask, "WaitTimeSeconds": wait, "VisibilityTimeout": VISIBILITY_SECS }),
            )?;
            let messages = answer.get("Messages").and_then(Value::as_array).cloned().unwrap_or_default();
            if messages.is_empty() {
                break;
            }
            for m in messages {
                let Some(receipt) = m.get("ReceiptHandle").and_then(Value::as_str) else { continue };
                match parse_body(m.get("Body").and_then(Value::as_str).unwrap_or_default()) {
                    Some(parsed) if parsed.is_empty() => self.ack(receipt)?,
                    Some(parsed) => deliveries.extend(parsed.into_iter().map(|message| Delivery { receipt: receipt.to_string(), message })),
                    None => {}
                }
            }
        }
        Ok(deliveries)
    }

    /// Delete the message behind `receipt`; a handle SQS no longer knows is no error.
    fn ack(&self, receipt: &str) -> Result<(), Failure> {
        match self.call("DeleteMessage", &json!({ "QueueUrl": self.queue_url, "ReceiptHandle": receipt }))? {
            Ok(_) => Ok(()),
            Err((kind, _)) if STALE_RECEIPTS.contains(&kind.as_str()) => Ok(()),
            Err((_, failure)) => Err(failure),
        }
    }

    fn message_cap(&self) -> usize {
        MESSAGE_CAP
    }
}
