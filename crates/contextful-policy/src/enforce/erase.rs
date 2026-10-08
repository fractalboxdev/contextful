//! Destructive admission carries verified authority, never a Read or owner inference.

use crate::verify::AdmittedAuthority;
use contextful_core::disclosure::erase::ErasureError;
use contextful_core::grant::{Action, Grant};
use std::collections::BTreeSet;

/// Only verified authority admitting every requested table constructs this value.
/// Transaction publication revalidates the carried authority through the verifier.
#[derive(Debug)]
pub struct ForgetAdmission<'a> {
    authority: &'a AdmittedAuthority,
    tables: BTreeSet<String>,
}

impl<'a> ForgetAdmission<'a> {
    pub fn admit(authority: &'a AdmittedAuthority, tables: &[&str]) -> Result<Self, ErasureError> {
        if tables.is_empty() {
            return Err(ErasureError::ErasureScopeUnsupported("the request names no target tables".into()));
        }
        let tables: BTreeSet<String> = tables.iter().map(|table| (*table).to_owned()).collect();
        for table in &tables {
            let mut covering = authority.grants().iter().filter(|grant| {
                grant.actions.contains(&Action::Forget) && grant.tables.iter().any(|pattern| pattern.covers_name(table))
            }).peekable();
            if covering.peek().is_none() {
                return Err(ErasureError::ErasureUngranted(table.clone()));
            }
            if !covering.any(unrestricted) {
                return Err(ErasureError::ErasureScopeUnsupported(format!("table `{table}` has only narrowed Forget grants")));
            }
        }
        Ok(Self { authority, tables })
    }

    pub fn authority(&self) -> &'a AdmittedAuthority {
        self.authority
    }

    pub fn tables(&self) -> impl Iterator<Item = &str> {
        self.tables.iter().map(String::as_str)
    }
}

fn unrestricted(grant: &Grant) -> bool {
    grant.tenant.is_none() && grant.aggregate.is_none() && grant.templates.is_none()
        && grant.max_rows.is_none() && grant.max_duration_ms.is_none() && grant.max_response_bytes.is_none()
}
