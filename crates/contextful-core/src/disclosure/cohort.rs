//! `disclosure.bound-cohort`: the line between a per-person model and a cohort model.
//!
//! A governed model declares `governance`, `per-person` by default. A `cohort` model's key
//! names an attribute many contributors share, so a key naming the contributor key or a
//! forbidden column resolves to one person and refuses; a `per-person` model never
//! publishes exact figures. Both checks read the declaration alone, before any row.

use super::DisclosureError;
use crate::pipeline::model::{DisclosureDecl, Figures, Governance};

/// Admits `policy`'s governance over a model whose grain is `grain`.
pub fn admit_governance(policy: &DisclosureDecl, grain: &[String]) -> Result<(), DisclosureError> {
    match policy.governance() {
        Governance::PerPerson => {
            if policy.figures() == Figures::Exact {
                return Err(DisclosureError::ExactPerPerson(
                    "a `per-person` model declares `figures = \"exact\"`; exact figures publish only under `governance = \"cohort\"`".into(),
                ));
            }
        }
        Governance::Cohort => {
            let contributor = policy.contributor_key.as_str();
            let key = policy.grouping_allowlist.iter().chain(grain.iter().filter(|c| *c != contributor));
            if let Some(c) = key.into_iter().find(|c| *c == contributor || policy.forbidden_columns.contains(c)) {
                return Err(DisclosureError::SingletonCohort(format!(
                    "cohort key column `{c}` names one person: it is the contributor key or a forbidden column"
                )));
            }
        }
    }
    Ok(())
}
