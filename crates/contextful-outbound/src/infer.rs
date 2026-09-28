//! The one inference endpoint: an operator-configured URL speaking OpenAI-compatible chat
//! completions. The host holds the credential; the caller supplies the conversation. Every
//! completion sends through the mediated client, its allowlist the endpoint host alone
//! (`connector.infer.mediated-call`).

use crate::client::{Client, HeaderValue};
use crate::egress::{PreSendHook, Transport};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::reference::Hydrated;
use contextful_core::memory::synthesize::{Inference, Message};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Wall clock one completion may take.
pub const COMPLETION_TIMEOUT: Duration = Duration::from_secs(120);

/// Largest completion body read.
const MAX_COMPLETION_BYTES: u64 = 16 * 1024 * 1024;

pub struct Endpoint {
    completions: Url,
    model: String,
    key: Option<Hydrated>,
    client: Client,
}

impl Endpoint {
    /// An endpoint at `base` (the URL `/chat/completions` hangs off) serving `model`,
    /// authenticated by a bearer `key` where one is configured.
    pub fn new(base: &str, model: &str, key: Option<String>) -> Result<Endpoint, String> {
        let base = Url::parse(base).map_err(|e| format!("the inference endpoint `{base}` is no URL: {e}"))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(format!("the inference endpoint speaks http or https, not `{}`", base.scheme()));
        }
        let completions = Url::parse(&format!("{}/chat/completions", base.as_str().trim_end_matches('/'))).map_err(|e| e.to_string())?;
        let host = completions.host_str().unwrap_or_default().to_string();
        let allow = Allowlist::parse(&[host]).map_err(|e| e.to_string())?;
        let client = Client::new(allow, completions.clone()).with_completion_timeout(COMPLETION_TIMEOUT).with_body_limit(MAX_COMPLETION_BYTES);
        let key = key.filter(|k| !k.is_empty()).map(|k| Hydrated::new(format!("Bearer {k}")));
        Ok(Endpoint { completions, model: model.to_string(), key, client })
    }

    /// The endpoint reaching the network through `transport`.
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Endpoint {
        self.client = self.client.with_transport(transport);
        self
    }

    /// The endpoint passing each completion through the operator's pre-send `hook`.
    pub fn with_hook(mut self, hook: Arc<dyn PreSendHook>) -> Endpoint {
        self.client = self.client.with_hook(hook);
        self
    }
}

impl Inference for Endpoint {
    fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let body = json!({ "model": self.model, "messages": messages, "temperature": 0 }).to_string();
        let mut headers = vec![("Content-Type".to_string(), HeaderValue::Plain("application/json".into()))];
        if let Some(key) = &self.key {
            headers.push(("Authorization".to_string(), HeaderValue::Sensitive(key.clone())));
        }
        let resp = self.client.send("POST", &self.completions, &headers, Some(body.as_bytes())).map_err(|f| f.message)?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("the endpoint answered {}: {}", resp.status, String::from_utf8_lossy(&resp.body).chars().take(200).collect::<String>()));
        }
        let v: Value = serde_json::from_slice(&resp.body).map_err(|e| format!("the endpoint's answer is not JSON: {e}"))?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the endpoint's answer carries no `choices[0].message.content`".to_string())
    }
}
