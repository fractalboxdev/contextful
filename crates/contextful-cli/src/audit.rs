//! `contextful audit` — verify, prove and query a project's audit chain at
//! `.contextful/audit/` (`disclosure.record.read-chain`).
//!
//! A thin adapter over `contextful_policy::audit` and the `audit_reads` projection. Every
//! verb but `anchor` opens the chain read-only and takes no writer lock; every chain verb
//! answers the local store owner alone: a capability credential in the environment refuses before any audit file is
//! read (`disclosure.attest.owner-only`).

use crate::admit::{static_pins, TOKEN_VAR};
use crate::project::{locate, pipeline_files};
use anyhow::{Context, Result};
use contextful_context::read::{audit_reads, ReadOptions};
use contextful_policy::audit::{self, AuditError, AuditLog, InclusionProof};
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_policy::keyset::KeySource;
use contextful_core::disclosure::declare::Binding;
use contextful_core::exchange::ExchangePolicy;
use contextful_core::store::declare::TableDecl;
use contextful_policy::explain::{self, Explanation, Window};
use std::path::{Path, PathBuf};

#[derive(clap::Subcommand)]
pub enum AuditCmd {
    /// Verify the chain's digests, linkage and roots, and, under `--public-key`, every
    /// signature; print the chain end.
    Verify {
        #[arg(long)]
        project: Option<String>,
        /// Issuer key pins the roots and the tip verify under; absent checks linkage alone.
        #[arg(long)]
        public_key: Option<String>,
    },
    /// Sign an unanchored chain's missing roots and its tip through the issuer's seed,
    /// and print the chain end.
    Anchor {
        #[arg(long)]
        project: Option<String>,
        /// The issuer's private seed file `token keygen` wrote.
        #[arg(long)]
        issuer_key: PathBuf,
    },
    /// Print the inclusion proof of one entry.
    Prove {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        seq: u64,
    },
    /// Verify an inclusion proof offline under the signer's public key alone.
    CheckProof {
        /// The proof file `audit prove` printed.
        #[arg(long)]
        proof: PathBuf,
        #[arg(long)]
        public_key: String,
    },
    /// Explain an access decision and its path, replay a window of the chain, or report a
    /// table's audience, with a coverage block and no row (`disclosure.explain`).
    #[command(group(clap::ArgGroup::new("about").required(true).args(["table", "audience"])))]
    Explain {
        #[arg(long)]
        project: Option<String>,
        /// The table to decide about; takes `--subject`.
        #[arg(long, requires = "subject")]
        table: Option<String>,
        /// The principal the decision and the replay are about.
        #[arg(long)]
        subject: Option<String>,
        /// A role the principal's verified assertion carries; repeatable.
        #[arg(long = "role")]
        roles: Vec<String>,
        /// `<from>..<to>`, two RFC 3339 instants: replay the chain over this window.
        #[arg(long)]
        window: Option<String>,
        /// Report who reaches this table instead of deciding.
        #[arg(long, conflicts_with_all = ["table", "subject", "roles"])]
        audience: Option<String>,
        /// The pipeline declaration naming the table's steps.
        #[arg(long)]
        declaration: Option<PathBuf>,
    },
    /// Run one statement over `audit_reads`, projected from the chain on this call.
    Query {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        sql: String,
        #[arg(long)]
        limit: Option<u64>,
    },
}

/// Refuse a verb run under a capability credential (`disclosure.attest.owner-only`).
fn owner_only(verb: &str) -> Result<(), AuditError> {
    match std::env::var(TOKEN_VAR) {
        Ok(t) if !t.trim().is_empty() => Err(AuditError::AuditRequiresOwner(format!(
            "`audit {verb}` answers the local store owner; unset {TOKEN_VAR}, which presents a capability credential"
        ))),
        _ => Ok(()),
    }
}

/// The signer keys a pin list names.
fn signer_keys(pins: &str) -> Result<Vec<SignerKey>> {
    let set = static_pins(Some(pins))?.keys()?;
    Ok(set.keys().map(|k| SignerKey { algorithm: k.algorithm(), public_key: k.public_key.to_bytes() }).collect())
}

/// The chain directory of the located project.
fn chain(project: Option<String>) -> Result<PathBuf> {
    Ok(locate(project.as_deref(), None)?.project.audit_dir())
}

/// Run `check` under each key, returning the first success, else the first key's refusal.
fn under_any<T>(keys: &[SignerKey], check: impl Fn(&SignerKey) -> Result<T, AuditError>) -> Result<T, AuditError> {
    let mut first = None;
    for key in keys {
        match check(key) {
            Ok(t) => return Ok(t),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(first.unwrap_or_else(|| AuditError::Io("no key to verify under".into())))
}

pub fn run(cmd: AuditCmd) -> Result<()> {
    match cmd {
        AuditCmd::Verify { project, public_key } => {
            owner_only("verify")?;
            let dir = chain(project)?;
            let end = match public_key {
                Some(pins) => under_any(&signer_keys(&pins)?, |k| audit::verify_signed(&dir, k))?,
                None => audit::verify(&dir)?,
            };
            println!("{}", serde_json::to_string(&end)?);
        }
        AuditCmd::Anchor { project, issuer_key } => {
            owner_only("anchor")?;
            let dir = chain(project)?;
            let log = AuditLog::anchor(&dir, SeedSigner::resolve(Some(&issuer_key))?)?;
            log.export()?;
            drop(log);
            println!("{}", serde_json::to_string(&audit::verify(&dir)?)?);
        }
        AuditCmd::Prove { project, seq } => {
            owner_only("prove")?;
            let proof = audit::prove(&chain(project)?, seq)?;
            println!("{}", serde_json::to_string(&proof)?);
        }
        AuditCmd::CheckProof { proof, public_key } => {
            let text = std::fs::read_to_string(&proof).with_context(|| format!("reading {}", proof.display()))?;
            let proof: InclusionProof = serde_json::from_str(&text)
                .map_err(|e| AuditError::AuditProofInvalid(format!("{} does not read as a proof: {e}", proof.display())))?;
            under_any(&signer_keys(&public_key)?, |k| proof.verify(k))?;
            println!("{}", serde_json::json!({ "seq": proof.entry.seq, "entry_hash": proof.entry.entry_hash }));
        }
        AuditCmd::Explain { project, table, subject, roles, window, audience, declaration } => {
            owner_only("explain")?;
            let out = explain(Question { project, table, subject, roles, window, audience, declaration })?;
            println!("{}", serde_json::to_string(&out)?);
        }
        AuditCmd::Query { project, sql, limit } => {
            owner_only("query")?;
            let entries = audit::entries(&chain(project)?)?;
            let response = audit_reads(&entries, &sql, ReadOptions { limit, ..ReadOptions::default() })?;
            println!("{}", serde_json::to_string(&response.to_json())?);
        }
    }
    Ok(())
}

/// The segment files under a chain directory.
fn segment_count(dir: &Path) -> u64 {
    std::fs::read_dir(dir.join("segments"))
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl")).count() as u64)
        .unwrap_or(0)
}

/// One `audit explain` invocation's arguments.
struct Question {
    project: Option<String>,
    table: Option<String>,
    subject: Option<String>,
    roles: Vec<String>,
    window: Option<String>,
    audience: Option<String>,
    declaration: Option<PathBuf>,
}

/// Read `path`, or `None` where it does not exist.
fn read_if_present(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
    }
}

/// Answer one `audit explain` from the exchange policy, the declaration and the verified
/// chain; nothing reads the table's rows.
fn explain(q: Question) -> Result<serde_json::Value> {
    let located = locate(q.project.as_deref(), q.declaration)?;
    let window = q.window.map(|w| Window::parse(&w).map_err(anyhow::Error::msg)).transpose()?;
    let policy = read_if_present(&located.project.dir.join(ExchangePolicy::PATH))?.map(|t| ExchangePolicy::parse(&t)).transpose()?;
    // The declaration set (`store.declare.declaration-set`), as the read face reads it.
    let text = read_if_present(&located.declaration)?.unwrap_or_default();
    let decls = TableDecl::parse_declaration_set(&text, &pipeline_files(&located.declaration)?)
        .with_context(|| format!("`{}`", located.declaration.display()))?;
    let name = q.table.or(q.audience.clone()).unwrap_or_default();
    let decl = decls.iter().find(|d| d.name == name).cloned().unwrap_or_else(|| TableDecl::named(&name));

    let dir = located.project.audit_dir();
    audit::verify(&dir)?;
    let entries = audit::entries(&dir)?;
    let bindings: Vec<Binding> = Binding::of(&decl)?.into_iter().collect();
    let grants = policy.is_some().then(|| ExchangePolicy::PATH.to_string());
    let now = explain::OffsetDateTime::now_utc();
    let coverage = explain::coverage(&entries, segment_count(&dir), window.as_ref(), now, &bindings, grants);

    let mut e = Explanation { table: name.clone(), subject: q.subject, decision: None, replay: None, audience: None, coverage: Some(coverage) };
    if q.audience.is_some() {
        e.audience = Some(explain::audience(policy.as_ref(), &name, &entries, window.as_ref())?);
    } else {
        let steps = explain::steps(&decl, policy.as_ref().and_then(|p| p.tenant_claim.as_deref()))?;
        e.decision = Some(explain::decide(policy.as_ref(), &q.roles, &name, steps));
        if let (Some(w), Some(who)) = (&window, &e.subject) {
            e.replay = Some(explain::replay(&entries, who, &name, w)?);
        }
    }
    // No membership source reaches the store, so no group on the path resolves to members.
    Ok(e.seal(&|_| Vec::new())?)
}
