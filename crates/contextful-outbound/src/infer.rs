//! The one inference endpoint: an operator-configured URL speaking OpenAI-compatible chat
//! completions. The host holds the credential; the caller supplies the conversation. Every
//! completion sends through the mediated client, its allowlist the endpoint host alone
//! (`connector.infer.mediated-call`).

use crate::client::{Client, HeaderValue};
use crate::egress::{PreSendHook, Transport};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::error::ConnectorError;
use contextful_core::time::Instant;
use contextful_core::connector::reference::Hydrated;
use contextful_core::memory::synthesize::{CallRecord, Inference, Message, Usage};
use contextful_core::trace::{Span, SpanKind, SpanSink, TraceContext, TRACEPARENT, TRACESTATE};
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
    key: Option<Arc<Hydrated>>,
    client: Client,
    hosted_tools: Vec<String>,
    horizon: Option<Instant>,
    /// Whether the endpoint exposes token log-probabilities, which each call then asks for.
    logprobs: bool,
    /// The span each call parents under, and where its span goes (`connector.infer.model-span`).
    trace: Option<(TraceContext, Arc<dyn SpanSink>)>,
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
        let client = Client::new(allow, completions.clone()).with_completion_timeout(COMPLETION_TIMEOUT).with_body_limit(MAX_COMPLETION_BYTES).framed("Content-Type", "application/json");
        let key = key.filter(|k| !k.is_empty()).map(|k| Arc::new(Hydrated::new(format!("Bearer {k}"))));
        Ok(Endpoint { completions, model: model.to_string(), key, client, hosted_tools: Vec::new(), horizon: None, logprobs: false, trace: None })
    }

    /// The endpoint reaching the network through `transport`.
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Endpoint {
        self.client = self.client.with_transport(transport);
        self
    }

    /// The endpoint declaring provider-hosted `tools`, such as `web_search`, on each completion.
    pub fn with_hosted_tools(mut self, tools: Vec<String>) -> Endpoint {
        self.hosted_tools = tools;
        self
    }

    /// The endpoint serving a caller whose credential carries `horizon`
    /// (`connector.infer.hosted-tools`).
    /// Its client reaches the endpoint host alone (`read.horizon.egress-hook`).
    pub fn under_horizon(mut self, horizon: Option<Instant>) -> Endpoint {
        self.horizon = horizon;
        if horizon.is_some() {
            let host = self.completions.host_str().map(str::to_string);
            self.client = self.client.under_horizon(host.as_deref());
        }
        self
    }

    /// The endpoint exposing token log-probabilities: each call asks for them and lands
    /// them with its record (`connector.infer.logprobs`).
    pub fn with_logprobs(mut self) -> Endpoint {
        self.logprobs = true;
        self
    }

    /// The endpoint recording each call as a child span of `parent` into `sink`, its request
    /// carrying a `traceparent` naming that span (`connector.infer.model-span`).
    pub fn traced(mut self, parent: TraceContext, sink: Arc<dyn SpanSink>) -> Endpoint {
        self.trace = Some((parent, sink));
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
        self.call(messages).map(|r| r.content)
    }

    fn call(&self, messages: &[Message]) -> Result<CallRecord, String> {
        let Some((parent, sink)) = &self.trace else { return self.exchange(messages, None).map(|(r, _)| r) };
        let context = parent.child(crate::telemetry::span_id());
        let start = crate::telemetry::now_unix_nanos();
        let outcome = self.exchange(messages, Some(&context));
        let mut span = Span::at(&context, Some(parent.span_id), &format!("chat {}", self.model), SpanKind::Client)
            .with("gen_ai.operation.name", "chat")
            .with("gen_ai.request.model", self.model.as_str())
            .with("server.address", self.completions.host_str().unwrap_or_default());
        if let Some(port) = self.completions.port_or_known_default() {
            span = span.with("server.port", u64::from(port));
        }
        match &outcome {
            Ok((record, model)) => {
                if let Some(model) = model {
                    span = span.with("gen_ai.response.model", model.as_str());
                }
                if let Some(usage) = record.usage {
                    span = span.with("gen_ai.usage.input_tokens", usage.input_tokens).with("gen_ai.usage.output_tokens", usage.output_tokens);
                }
            }
            Err(e) => {
                let kind = e.split(|c: char| c == ':' || c.is_whitespace()).next().filter(|k| k.chars().next().is_some_and(char::is_uppercase)).unwrap_or("_OTHER");
                span = span.with("error.type", kind);
                span.error = Some(kind.to_string());
            }
        }
        span.start_unix_nanos = start;
        span.end_unix_nanos = crate::telemetry::now_unix_nanos();
        sink.record(span);
        outcome.map(|(r, _)| r)
    }
}

impl Endpoint {
    /// One completion: the call record and the model the endpoint answered as. A traced call
    /// carries `context` on the wire as `traceparent`, beside any `tracestate`.
    fn exchange(&self, messages: &[Message], context: Option<&TraceContext>) -> Result<(CallRecord, Option<String>), String> {
        // A hosted tool reaches the network inside the provider, past every hook, so a
        // horizon refuses it before any byte leaves.
        if let (Some(horizon), Some(tool)) = (self.horizon, self.hosted_tools.first()) {
            return Err(ConnectorError::InferHostedToolUnderHorizon(format!("the call declares the provider-hosted tool `{tool}` under the horizon {horizon}")).to_string());
        }
        let mut body = json!({ "model": self.model, "messages": messages, "temperature": 0 });
        if !self.hosted_tools.is_empty() {
            body["tools"] = Value::Array(self.hosted_tools.iter().map(|t| json!({ "type": t })).collect());
        }
        if self.logprobs {
            body["logprobs"] = json!(true);
        }
        let body = body.to_string();
        // The configured key is the call's one declared header: without it the call keeps the system proxy.
        let mut headers: Vec<(String, HeaderValue)> = self.key.iter().map(|k| ("Authorization".to_string(), HeaderValue::Sensitive(k.clone()))).collect();
        if let Some(context) = context {
            headers.push((TRACEPARENT.to_string(), HeaderValue::Plain(context.traceparent())));
            headers.extend(context.state.clone().map(|state| (TRACESTATE.to_string(), HeaderValue::Plain(state))));
        }
        let resp = self.client.send("POST", &self.completions, &headers, Some(body.as_bytes())).map_err(|f| f.message)?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("the endpoint answered {}: {}", resp.status, String::from_utf8_lossy(&resp.body).chars().take(200).collect::<String>()));
        }
        let v: Value = serde_json::from_slice(&resp.body).map_err(|e| format!("the endpoint's answer is not JSON: {e}"))?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the endpoint's answer carries no `choices[0].message.content`".to_string())?;
        let logprobs = Some(v["choices"][0]["logprobs"].clone()).filter(|l| self.logprobs && !l.is_null());
        // OpenAI-compatible endpoints count `prompt_tokens` and `completion_tokens`.
        let tokens = |a: &str, b: &str| v["usage"][a].as_u64().or_else(|| v["usage"][b].as_u64());
        let usage = match (tokens("prompt_tokens", "input_tokens"), tokens("completion_tokens", "output_tokens")) {
            (Some(input_tokens), Some(output_tokens)) => Some(Usage { input_tokens, output_tokens }),
            _ => None,
        };
        let model = v["model"].as_str().map(str::to_string);
        Ok((CallRecord { content, logprobs, usage }, model))
    }
}
