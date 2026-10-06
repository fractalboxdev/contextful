//! `contextful serve --http` — the tool server over MCP Streamable HTTP.
//!
//! A thin adapter: it checks the declared audience and in-flight ceiling and the issuer
//! key, pulls the bucket when `[sync] pull_before_run = true`, opens the read face over
//! the project's store and manifest and the project's audit chain unanchored
//! (`disclosure.record.read-chain`), binds the listener, and hands every request to the
//! network transport, which admits each one on its own credential. Every value is
//! resolved before the listener binds, so a process that cannot serve binds nothing.

use crate::admit::{face, revocation_state, LedgerFile, LivePins, AUDIENCE_VAR, PUBKEY_VAR};
use crate::project::locate;
use crate::root::root as project_root;
use crate::clock::SystemClock;
use anyhow::Result;
use contextful_agent::http::{audience, ceiling, Admitting, HttpFace, HttpRequest, HttpResponse, APPLY_PATH, EDIT_PATH, RECORD_PATH, WORKFLOWS_PATH};
use contextful_core::surface::SurfaceError;
use contextful_core::run::derive::task::Tasks;
use contextful_engine::control::ControlError;
use contextful_core::issue::{IssuancePolicy, MintContext, NodeRole};
use contextful_core::ports::{Clock, SigningPort};
use contextful_policy::exchange::answer as exchange_answer;
use contextful_policy::issue::{SeedSigner, DEFAULT_SEED_PATH};
use contextful_policy::possession::ProofChecker;
use contextful_policy::audit::AuditLog;
use contextful_policy::verify::AdmittedAuthority;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use contextful_policy::keyset::{KeyCheckpoint, StaticPins};
use contextful_policy::revoke::RevocationState;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use serde_json::{json, Value};

/// The refusals of starting the network transport. `Display` begins with the identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServeError {
    /// (`topology.publish-hostname.issuer-key`)
    #[error("IssuerKeyUnusable: {0}")]
    IssuerKeyUnusable(String),
}

#[derive(clap::Args)]
pub struct ServeArgs {
    /// The address the transport listens on, such as `127.0.0.1:8080`; it answers
    /// MCP Streamable HTTP at `POST /mcp`.
    #[arg(long)]
    http: String,
    /// The audience every credential names. Required, with no default.
    #[arg(long, env = AUDIENCE_VAR)]
    audience: Option<String>,
    /// Requests in flight at once; past it a request answers `503`. Required, with no default.
    #[arg(long)]
    max_in_flight: Option<usize>,
    /// Bytes the result cache holds, least recently used evicted first; absent, no result
    /// caches (`read.cache.budget`).
    #[arg(long)]
    result_cache_bytes: Option<u64>,
    /// The project whose store root is `.contextful/context/<project>/` under the working
    /// directory; absent, the nearest `contextful.toml` upward names it.
    #[arg(long)]
    project: Option<String>,
    /// The pipeline manifest holding the table declarations and query templates; absent,
    /// the project's `contextful.toml`.
    #[arg(long)]
    declaration: Option<PathBuf>,
    /// Comma-separated issuer key pins.
    #[arg(long, env = PUBKEY_VAR)]
    public_key: Option<String>,
    /// A file of revocation identifiers, one per line, re-read on every request.
    #[arg(long)]
    denylist: Option<PathBuf>,
    /// The key-set ledger holding retired keys and scoped epochs, its epochs re-read on
    /// every request; absent, `.contextful/keyset.toml` under the project root.
    #[arg(long)]
    keyset: Option<PathBuf>,
}

fn operator_attestation(request: &HttpRequest, secret: &str, used: &Mutex<HashMap<String, i64>>) -> Option<String> {
    let subject = request.header("X-Contextful-Operator")?;
    let at: i64 = request.header("X-Contextful-Operator-Time")?.parse().ok()?;
    let nonce = request.header("X-Contextful-Operator-Nonce")?;
    let signature = request.header("X-Contextful-Operator-Signature")?;
    if subject.is_empty() || subject.len() > 256 || nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) || signature.len() != 64 {
        return None;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    if now.abs_diff(at) > 60 { return None; }
    let digest = format!("{:x}", Sha256::digest(&request.body));
    let message = format!("{}\n{}\n{digest}\n{subject}\n{at}\n{nonce}", request.method, request.target);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(message.as_bytes());
    let mut signature_bytes = [0u8; 32];
    for (index, chunk) in signature.as_bytes().chunks_exact(2).enumerate() {
        signature_bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    mac.verify_slice(&signature_bytes).ok()?;
    let mut seen = used.lock().ok()?;
    seen.retain(|_, expires| *expires >= now);
    if seen.len() >= 10_000 || seen.insert(format!("{subject}:{nonce}"), at + 60).is_some() { return None; }
    Some(subject.to_owned())
}

/// The issuer key pins, resolved and parsed before the listener binds; no generated key
/// substitutes (`topology.publish-hostname.issuer-key`).
fn issuer_pins(flag: Option<&str>) -> Result<StaticPins, ServeError> {
    let Some(text) = flag.filter(|t| !t.trim().is_empty()) else {
        return Err(ServeError::IssuerKeyUnusable(format!("no issuer key: pass --public-key or set {PUBKEY_VAR}")));
    };
    StaticPins::parse(text).map_err(|e| ServeError::IssuerKeyUnusable(format!("--public-key / {PUBKEY_VAR}: {e}")))
}

pub fn run(args: ServeArgs, tasks: &Tasks) -> Result<()> {
    let clock = SystemClock;
    // The declarations are checked before anything opens (`read.register.serve-declaration`).
    let audience = audience(args.audience.as_deref()).map_err(anyhow::Error::msg)?;
    let ceiling = ceiling(args.max_in_flight).map_err(anyhow::Error::msg)?;
    let root = project_root(args.project.as_deref())?;
    let ledger = Arc::new(LedgerFile::at(&root, args.keyset.as_deref()));
    // Every admission filters the pins through the ledger it reads then, so a key retired
    // while the face runs verifies nothing from the next request; a start with every
    // pinned key retired, or an unreadable ledger, binds nothing.
    let pins = issuer_pins(args.public_key.as_deref())?;
    let live = LivePins::new(pins, ledger.clone(), SystemClock);
    let checkpoint = KeyCheckpoint::start(Box::new(live), SystemClock).map_err(|e| ServeError::IssuerKeyUnusable(e.to_string()))?;
    let denylist = args.denylist.clone();
    let revocation = move || -> Result<RevocationState, String> {
        let ledger = ledger.read().map_err(|e| e.to_string())?;
        revocation_state(denylist.as_deref(), &ledger).map_err(|e| format!("{e:#}"))
    };
    // The denylist reads once before binding, so a missing file refuses the start.
    revocation().map_err(anyhow::Error::msg)?;
    let admitting = Admitting { checkpoint: &checkpoint, audience, revocation: &revocation };
    let located = locate(args.project.as_deref(), args.declaration)?;
    let text = std::fs::read_to_string(&located.declaration).unwrap_or_default();
    // Every configured resource resolves inside the residency allow-set before the listener
    // binds (`surface.reside.region-mismatch`).
    crate::reside::enforce(&located, &text)?;
    // A cold node pulls the bucket before the face opens; a failed pull binds nothing
    // (`store.pull.before-run`).
    crate::sync::pull_before_run(&located)?;
    let face = face(&located)?;
    let face = match args.result_cache_bytes {
        Some(budget) => face.with_result_cache(budget),
        None => face,
    };
    let audit = AuditLog::unanchored(located.project.audit_dir())?;
    let control_attestation_secret = std::env::var("CONTEXTFUL_CONTROL_ATTESTATION_SECRET").ok().filter(|secret| !secret.is_empty());
    let attestations = Mutex::new(HashMap::new());
    let control = |request: &HttpRequest, authority: &AdmittedAuthority| -> HttpResponse {
        let malformed = || HttpResponse::json(400, &json!({ "error": { "identifier": "ControlRequestMalformed" } }));
        let path = request.target.split('?').next().unwrap_or_default();
        let operator = if path == EDIT_PATH || path == APPLY_PATH {
            match control_attestation_secret.as_deref() {
                Some(secret) => match operator_attestation(request, secret, &attestations) {
                    Some(subject) => subject,
                    None => return HttpResponse::json(403, &json!({ "error": { "identifier": "ControlOperatorAttestationInvalid" } })),
                },
                None => return HttpResponse::json(403, &json!({ "error": { "identifier": "ControlOperatorAttestationInvalid" } })),
            }
        } else { String::new() };
        let answer = match path {
            WORKFLOWS_PATH => crate::cadence::published(&located.project, &located.declaration),
            RECORD_PATH => contextful_policy::audit::entries(&located.project.audit_dir())
                .map(|entries| {
                    let count = entries.len();
                    json!({ "entries": entries.into_iter().rev().take(1000).collect::<Vec<_>>(), "truncated": count > 1000, "declined": count.saturating_sub(1000) })
                }).map_err(anyhow::Error::from),
            EDIT_PATH | APPLY_PATH => {
                let body: Value = match serde_json::from_slice(&request.body) {
                    Ok(Value::Object(body)) => Value::Object(body),
                    _ => return malformed(),
                };
                let Some(fields) = body.as_object() else { unreachable!() };
                let Some(expected) = fields.get("expected").and_then(Value::as_u64) else { return malformed() };
                let project = crate::run::ProjectArgs { project: Some(located.project.name.clone()), now: None };
                if path == EDIT_PATH {
                    if fields.len() != 2 || fields.keys().any(|field| field != "expected" && field != "document") {
                        return malformed();
                    }
                    let Some(document) = fields.get("document").and_then(Value::as_str) else { return malformed() };
                    if audit.append(json!({ "contextful.control.operation": "edit", "contextful.operator.subject": operator,
                        "contextful.operator.attestation": "console-hmac",
                        "contextful.credential": authority.credential_id(), "contextful.control.expected": expected })).is_err() {
                        return HttpResponse::json(503, &json!({ "error": { "identifier": "AuditEntryUnpersisted" } }));
                    }
                    crate::cadence::edit(&project, Some(located.declaration.clone()), expected, document, &operator, tasks)
                } else {
                    if fields.len() != 2 { return malformed(); }
                    let Some(nonce) = fields.get("nonce").and_then(Value::as_str) else { return malformed() };
                    if audit.append(json!({ "contextful.control.operation": "apply", "contextful.operator.subject": operator,
                        "contextful.operator.attestation": "console-hmac",
                        "contextful.credential": authority.credential_id(), "contextful.control.expected": expected,
                        "contextful.control.draft_nonce": nonce })).is_err() {
                        return HttpResponse::json(503, &json!({ "error": { "identifier": "AuditEntryUnpersisted" } }));
                    }
                    crate::cadence::apply_draft(&project, Some(located.declaration.clone()), expected, nonce, &operator, tasks)
                        .and_then(|()| crate::cadence::published(&located.project, &located.declaration))
                }
            }
            _ => unreachable!(),
        };
        match answer {
            Ok(state) => HttpResponse::json(200, &state),
            Err(error) => {
                let surface = error.chain().find_map(|part| part.downcast_ref::<SurfaceError>().or_else(|| {
                    match part.downcast_ref::<ControlError>() {
                        Some(ControlError::Surface(refusal)) => Some(refusal),
                        _ => None,
                    }
                }));
                let status = surface.map_or(503, SurfaceError::status);
                let identifier = surface.map(|refusal| refusal.to_string().split(':').next().unwrap_or("ControlUnavailable").to_string())
                    .unwrap_or_else(|| "ControlUnavailable".into());
                HttpResponse::json(status, &json!({ "error": { "identifier": identifier } }))
            }
        }
    };
    let exchange = crate::token::configured_exchange(&root)?;
    let mint_material = if exchange.is_some() {
        let policy = std::fs::read_to_string(root.join(IssuancePolicy::PATH))?;
        let issuance = IssuancePolicy::parse(&policy).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let signer = SeedSigner::resolve(Some(&root.join(DEFAULT_SEED_PATH)))?;
        Some((issuance, signer))
    } else {
        None
    };
    let proofs = ProofChecker::new(SystemClock);
    let exchange_route = |request: &contextful_agent::http::HttpRequest| {
        let Some((issuance, signer)) = mint_material.as_ref() else {
            return contextful_agent::http::HttpResponse::json(404,
                &serde_json::json!({ "error": { "http": 404, "identifier": "ExchangeUnconfigured", "message": ".contextful/exchange/policy.toml is absent" } }));
        };
        let ctx = MintContext { node: NodeRole::Primary, signer: signer as &dyn SigningPort, clock: &clock as &dyn Clock };
        let result = exchange_answer(exchange.as_ref(), &request.body, request.header("DPoP"), &proofs, issuance, &ctx);
        contextful_agent::http::HttpResponse::json(result.status, &result.body)
    };
    let http = HttpFace::new(&face, &clock, &audit, admitting, Some(ceiling))
        .map_err(anyhow::Error::msg)?
        .with_control(&control)
        .with_exchange(&exchange_route, exchange.is_none());
    let listener = TcpListener::bind(&args.http)?;
    eprintln!("listening on http://{}/mcp", listener.local_addr()?);
    http.serve(listener)?;
    Ok(())
}
