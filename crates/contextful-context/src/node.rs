//! Node identity from the process environment and the state directory
//! (`store.lay-out.node-id-order`).

use crate::error::Result;
use crate::store::Store;
use contextful_core::store::lay_out::{resolve_node_id, NodeId, NodeIdSource};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const NODE_ID_ENV: &str = "CONTEXTFUL_NODE_ID";
pub const STATE_DIR_ENV: &str = "CONTEXTFUL_STATE_DIR";
const NODE_ID_FILE: &str = "node-id";

/// The state directory: `$CONTEXTFUL_STATE_DIR`, else `$XDG_STATE_HOME/contextful`, else
/// the platform user-state directory (`store.lay-out.node-id-state-path`).
pub fn state_dir(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(d) = set(STATE_DIR_ENV) {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = set("XDG_STATE_HOME") {
        return Some(Path::new(&d).join("contextful"));
    }
    if cfg!(windows) {
        return set("LOCALAPPDATA").map(|d| Path::new(&d).join("contextful"));
    }
    let home = set("HOME")?;
    Some(if cfg!(target_os = "macos") {
        Path::new(&home).join("Library/Application Support/contextful")
    } else {
        Path::new(&home).join(".local/state/contextful")
    })
}

/// Read the host id persisted in `dir`, generating `node-<8 hex>` there on first use.
/// `None` where the directory is not writable (`store.lay-out.node-id-state-path`).
pub fn persisted_node_id(dir: &Path) -> Option<String> {
    let path = dir.join(NODE_ID_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        return Some(text.trim().to_string());
    }
    std::fs::create_dir_all(dir).ok()?;
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).ok()?;
    let id = format!("node-{}", crate::store::hex(&bytes));
    match crate::store::create_new_file(&path, format!("{id}\n").as_bytes()) {
        Ok(true) => Some(id),
        // Another process generated one first; take it.
        Ok(false) => std::fs::read_to_string(&path).ok().map(|t| t.trim().to_string()),
        Err(_) => None,
    }
}

/// `path` made absolute with its deepest existing ancestor canonicalized, so the store
/// root names one path whether or not its directories exist yet.
fn resolved_path(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut tail = Vec::new();
    let mut at = abs.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return tail.iter().rev().fold(real, |p, seg| p.join(seg));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                at = parent;
            }
            _ => return abs,
        }
    }
}

/// The node id `store` takes by default on the host whose id is `host_id`: `node-<8 hex>`
/// of SHA-256 over the host id and the absolute store root (`store.lay-out.node-id-project`).
pub fn project_node_id(host_id: &str, store: &Store) -> String {
    let root = resolved_path(store.root());
    let mut h = Sha256::new();
    h.update(host_id.as_bytes());
    h.update([0u8]);
    h.update(root.as_os_str().as_encoded_bytes());
    format!("node-{}", crate::store::hex(&h.finalize()[..4]))
}

/// Resolve this process's node id for `store`. A state directory inside the store root
/// is refused as a source, since a copied store would carry its identity.
pub fn resolve(store: &Store, env: impl Fn(&str) -> Option<String>) -> Result<(NodeId, NodeIdSource)> {
    let from_env = env(NODE_ID_ENV).filter(|v| !v.is_empty());
    let dir = state_dir(&env).filter(|d| !d.starts_with(store.root()));
    let derived = || dir.as_deref().and_then(persisted_node_id).map(|host| project_node_id(&host, store));
    Ok(resolve_node_id(from_env, store.configured_node_id(), derived)?)
}
