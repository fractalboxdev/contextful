//! The refusals of the visibility operations of the `disclosure` contract, one variant
//! per error identifier.
//!
//! A variant's `Display` begins with its identifier, so a surface printing the error
//! names the refusal a caller greps for.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VisibilityError {
    /// A staleness budget is compound, bare, or carries an unknown suffix.
    /// (`disclosure.bound-staleness.budget-grammar`)
    #[error("VisibilityBudgetMalformed: table `{table}`: max_acl_staleness `{text}` is not a positive integer with one suffix from s, m, h, d")]
    BudgetMalformed { table: String, text: String },
    /// A table declared a level its source family does not permit.
    /// (`disclosure.declare-fidelity.family-bound`)
    #[error("VisibilityFamilyBound: table `{table}`: family `{family}` does not permit fidelity `{fidelity}`")]
    FamilyBound { table: String, family: &'static str, fidelity: &'static str },
    /// A mapping landed a table without naming its family.
    /// (`disclosure.declare-fidelity.family-undeclared`)
    #[error("VisibilityFamilyUndeclared: table `{table}` names no family")]
    FamilyUndeclared { table: String },
}

impl VisibilityError {
    /// The error identifier.
    pub fn identifier(&self) -> &'static str {
        match self {
            VisibilityError::BudgetMalformed { .. } => "VisibilityBudgetMalformed",
            VisibilityError::FamilyBound { .. } => "VisibilityFamilyBound",
            VisibilityError::FamilyUndeclared { .. } => "VisibilityFamilyUndeclared",
        }
    }
}
