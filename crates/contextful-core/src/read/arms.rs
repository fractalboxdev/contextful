//! The arms of a ranked read: the session's relations under a prefix, narrowed by an
//! optional table list (`read.retrieve.table-list`).

use super::ReadError;

/// The relations a ranked read reads, in relation order: each name starting with
/// `prefix` and, when `tables` is given, listed in it. A listed name outside `relations`
/// raises `RetrieveTableUnavailable` naming every such string once, in list order, so an
/// absent table and an ungranted one refuse alike (`authority.filter-rows.default-deny`).
pub fn arms<'a>(relations: impl IntoIterator<Item = &'a str>, prefix: &str, tables: Option<&[String]>) -> Result<Vec<String>, ReadError> {
    let relations: Vec<&str> = relations.into_iter().collect();
    if let Some(listed) = tables {
        let mut missing: Vec<&str> = Vec::new();
        for t in listed.iter().map(String::as_str).filter(|t| !relations.contains(t)) {
            if !missing.contains(&t) {
                missing.push(t);
            }
        }
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|t| format!("`{t}`")).collect();
            return Err(ReadError::RetrieveTableUnavailable(format!("not tables this read can list: {}", names.join(", "))));
        }
    }
    Ok(relations
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .filter(|n| tables.is_none_or(|listed| listed.iter().any(|t| t == n)))
        .map(str::to_string)
        .collect())
}
