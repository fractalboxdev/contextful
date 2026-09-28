//! The one inference endpoint: an operator-configured URL speaking OpenAI-compatible chat
//! completions. The host holds the credential; the caller supplies the conversation.

use contextful_core::memory::synthesize::{Inference, Message};
use serde_json::{json, Value};
use std::time::Duration;
use url::Url;

/// Wall clock one completion may take.
pub const COMPLETION_TIMEOUT: Duration = Duration::from_secs(120);

/// Largest completion body read.
const MAX_COMPLETION_BYTES: u64 = 16 * 1024 * 1024;

pub struct Endpoint {
    completions: Url,
    model: String,
    key: Option<String>,
    agent: ureq::Agent,
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
        let config = ureq::Agent::config_builder().timeout_global(Some(COMPLETION_TIMEOUT)).http_status_as_error(false).build();
        Ok(Endpoint { completions, model: model.to_string(), key: key.filter(|k| !k.is_empty()), agent: ureq::Agent::new_with_config(config) })
    }
}

impl Inference for Endpoint {
    fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let body = json!({ "model": self.model, "messages": messages, "temperature": 0 }).to_string();
        let mut req = ureq::http::Request::builder()
            .method("POST")
            .uri(self.completions.as_str())
            .header("Content-Type", "application/json");
        if let Some(key) = &self.key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let request = req.body(body.into_bytes()).map_err(|e| e.to_string())?;
        let mut resp = self.agent.run(request).map_err(|e| format!("the completion request failed: {e}"))?;
        let status = resp.status().as_u16();
        let bytes = resp.body_mut().with_config().limit(MAX_COMPLETION_BYTES).read_to_vec().map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!("the endpoint answered {status}: {}", String::from_utf8_lossy(&bytes).chars().take(200).collect::<String>()));
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("the endpoint's answer is not JSON: {e}"))?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the endpoint's answer carries no `choices[0].message.content`".to_string())
    }
}
