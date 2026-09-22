//! `authority.revoke`, checkpoint side: the denylist, the scoped epoch and format
//! withdrawal, applied at admission and again at each effect boundary.
//!
//! Every derivation carries its own revocation identifier: the library's per-block
//! identifier, hex-encoded. The authority block's `rev.id` names the credential as a
//! whole; a denylist entry for either withdraws it, and an entry for a child's block
//! identifier leaves its parent admitted.

use crate::verify::AdmittedAuthority;
use contextful_core::revoke::{check_revoked, Denylist, Epochs, FormatWithdrawals, RevocationClaims};
use contextful_core::time::Instant;
use contextful_core::AuthorityError;

/// The principal class of a credential whose subject names a verified principal.
pub const PRINCIPAL_CLASS_DELEGATED: &str = "delegated";

/// The principal class of a credential naming no `on_behalf_of`.
pub const PRINCIPAL_CLASS_UNATTRIBUTED: &str = "unattributed";

/// What a checkpoint reads to withdraw authority ahead of expiry.
#[derive(Debug, Clone, Default)]
pub struct RevocationState {
    pub denylist: Denylist,
    pub epochs: Epochs,
    pub withdrawals: FormatWithdrawals,
}

impl RevocationState {
    /// Refuse a withdrawn format with `CredentialFormatWithdrawn`, then a denylisted or
    /// superseded credential with `AuthorityRevoked`.
    pub fn check(&self, admitted: &AdmittedAuthority, at: Instant) -> Result<(), AuthorityError> {
        self.withdrawals.check(admitted.format(), at)?;
        check_revoked(&revocation_claims(admitted), &self.denylist, &self.epochs)
    }
}

/// A denylist file: one revocation identifier per line, blank lines and `#` comments
/// skipped. Each entry records `key_version`, the key its credential verifies under.
pub fn parse_denylist(text: &str, key_version: &str) -> Denylist {
    let mut denylist = Denylist::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        denylist.deny(line, key_version);
    }
    denylist
}

/// The revocation view of an admitted credential. The project is its audience; the
/// tenant is the one tenant every grant shares, if any; the identifiers are the
/// credential's `rev.id` followed by each derivation's, root first.
pub fn revocation_claims(admitted: &AdmittedAuthority) -> RevocationClaims {
    let mut tenants = admitted.grants().iter().map(|g| g.tenant.as_ref().map(|t| t.value.clone()));
    let first = tenants.next().flatten();
    let tenant = if tenants.all(|t| t == first) { first } else { None };
    let principal_class = if admitted.subject().on_behalf_of().is_some() {
        PRINCIPAL_CLASS_DELEGATED
    } else {
        PRINCIPAL_CLASS_UNATTRIBUTED
    };
    let mut revocation_ids = vec![admitted.credential_id().to_string()];
    revocation_ids.extend(admitted.revocation_ids().iter().cloned());
    RevocationClaims {
        project: admitted.audience().to_string(),
        tenant,
        principal_class: principal_class.to_string(),
        revocation_ids,
        epoch: admitted.epoch(),
    }
}
