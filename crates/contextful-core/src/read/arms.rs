//! The arms of a ranked read: the session's relations under a prefix, narrowed by an
//! optional table list (`read.retrieve.table-list`).

use super::ReadError;

/// The relations a ranked read reads, in relation order: each name starting with
/// `prefix` and, when `tables` is given, listed in it. A listed name outside `relations`
/// raises `RetrieveTableUnavailable` naming that string alone, so an absent table and an
/// ungranted one refuse alike (`authority.filter-rows.default-deny`).
pub fn arms<'a>(relations: impl IntoIterator<Item = &'a str>, prefix: &str, tables: Option<&[String]>) -> Result<Vec<String>, ReadError> {
    let relations: Vec<&str> = relations.into_iter().collect();
    if let Some(listed) = tables {
        if let Some(missing) = listed.iter().find(|t| !relations.contains(&t.as_str())) {
            return Err(ReadError::RetrieveTableUnavailable(format!("`{missing}` is not a table this read can list")));
        }
    }
    Ok(relations
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .filter(|n| tables.is_none_or(|listed| listed.iter().any(|t| t == n)))
        .map(str::to_string)
        .collect())
}
