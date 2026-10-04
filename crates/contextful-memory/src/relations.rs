//! The operator's relation vocabulary migration over stored memory edges.

use crate::claims::{objects, require, Boundary};
use crate::MemoryFault;
use contextful_context::land::{land, Batch, RunContext};
use contextful_context::read::Face;
use contextful_core::grant::Action;
use contextful_core::memory::declare::Shape;
use contextful_core::store::bound_time::Bounds;
use contextful_core::store::lay_out::NodeId;
use contextful_core::store::reserve::Injection;
use contextful_core::time::Instant;
use contextful_policy::enforce::session::Request;
use contextful_policy::verify::AdmittedAuthority;
use serde_json::Value;
use std::collections::HashMap;

/// Rewrite each live edge using `from` under its existing `edge_id`. The declaration
/// admits both names during migration; the old name leaves it after the rewrite.
pub fn rename(
    face: &Face,
    authority: &AdmittedAuthority,
    from: &str,
    to: &str,
    node: &NodeId,
    now: Instant,
    boundary: &Boundary<'_>,
) -> Result<(String, usize), MemoryFault> {
    if from == to || from.trim().is_empty() || to.trim().is_empty() {
        return Err(MemoryFault::Invalid("relation names must be distinct and nonempty".into()));
    }
    let memory = face.memory();
    let table = memory.of_shape(Shape::Edges).ok_or_else(|| MemoryFault::Undeclared("the manifest declares no memory_edges table".into()))?;
    if !memory.admits_relation(from) || !memory.admits_relation(to) {
        return Err(MemoryFault::Undeclared(format!("the manifest must admit both `{from}` and `{to}` during a relation rename")));
    }
    require(authority, Action::Read, &table.name)?;
    require(authority, Action::Write, &table.name)?;
    let session = face.session(authority, &Request::default(), Bounds::default())?;
    let mut rows = Vec::new();
    for mut row in objects(&face.rows(&session, &table.name, None)?) {
        if row.get("rel_type").and_then(Value::as_str) != Some(from) {
            continue;
        }
        row.retain(|name, _| !name.starts_with('_'));
        row.insert("rel_type".into(), Value::String(to.into()));
        rows.push(row);
    }
    let count = rows.len();
    if count > 0 {
        boundary()?;
        let types = face.store().try_schema(&table.name)?.unwrap_or_default().columns.into_iter().map(|c| (c.name, c.ty)).collect::<HashMap<_, _>>();
        let context = RunContext {
            node: node.clone(),
            injection: Injection {
                run_id: format!("memory-relations-rename-{}", now.unix_nanos()),
                site_id: "memory".into(),
                batch_seq: Some(0),
                authored_by: authority.subject().on_behalf_of().map(str::to_string),
                taint: None,
            },
            committed_at: now,
        };
        land(face.store(), &face.decl(&table.name), &Batch { rows, types }, &context)?;
    }
    Ok((table.name.clone(), count))
}
