//! `contextful audit` — verify, prove and query a project's audit chain at
//! `.contextful/audit/` (`disclosure.record.read-chain`).
//!
//! A thin adapter over `contextful_policy::audit` and the `audit_reads` projection. Every
//! verb but `anchor` opens the chain read-only and takes no writer lock; every chain verb
//! answers the local store owner alone: a capability credential in the environment refuses before any audit file is
//! read (`disclosure.attest.owner-only`).

use crate::admit::{static_pins, TOKEN_VAR};
use crate::project::locate;
use anyhow::{Context, Result};
use contextful_context::read::{audit_reads, ReadOptions};
use contextful_policy::audit::{self, AuditError, AuditLog, InclusionProof};
use contextful_policy::issue::{SeedSigner, SignerKey};
use contextful_policy::keyset::KeySource;
use std::path::PathBuf;

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
        AuditCmd::Query { project, sql, limit } => {
            owner_only("query")?;
            let entries = audit::entries(&chain(project)?)?;
            let response = audit_reads(&entries, &sql, ReadOptions { limit, ..ReadOptions::default() })?;
            println!("{}", serde_json::to_string(&response.to_json())?);
        }
    }
    Ok(())
}
