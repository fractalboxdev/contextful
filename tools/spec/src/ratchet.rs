//! `corpus.registry.ceiling`: the ratchets `spec/terms/corpus.toml` records, each a count
//! the tree may lower and never raise.

use crate::corpus::*;
use anyhow::{bail, Result};

const FILE: &str = "spec/terms/corpus.toml";

/// One ratchet's live count: its key (`name`, or `table.name`), the count, and the
/// check and error a mismatch raises.
pub struct Live {
    pub key: String,
    pub count: usize,
    pub check: &'static str,
    pub code: &'static str,
}

/// The ceiling recorded under `key`, when one is.
pub fn ceiling(c: &Corpus, key: &str) -> Option<usize> {
    let table = &c.reg.fragments.get("corpus")?.ceiling;
    let value = match key.split_once('.') {
        Some((t, name)) => table.get(t)?.get(name)?,
        None => table.get(key)?,
    };
    value.as_integer().and_then(|v| usize::try_from(v).ok())
}

/// The finding a live count differing from its ceiling raises, or none.
pub fn compare(c: &Corpus, live: &Live) -> Option<Finding> {
    let msg = match ceiling(c, &live.key) {
        None => format!("ratchet `{}` records no ceiling; the live count is {}", live.key, live.count),
        Some(cap) if live.count > cap => format!("ratchet `{}` counts {}, over its ceiling {cap}", live.key, live.count),
        Some(cap) if live.count < cap => {
            format!("ratchet `{}` counts {}, under its ceiling {cap}; run `contextful-spec ceilings`", live.key, live.count)
        }
        Some(_) => return None,
    };
    Some(Finding::new(live.check, FILE, 0, live.code, msg))
}

/// Every ratchet's live count.
pub fn all(c: &Corpus) -> Vec<Live> {
    let mut out = vec![crate::drift::committed_sites(c), crate::checks::long_lists(c), crate::size::unowned_live(c), crate::records::unscoped_live(c), crate::terms::first_use_live(c)];
    out.extend(crate::size::contract_words(c));
    out.extend(crate::size::adr_words(c));
    out
}

/// Lower each ceiling above its live count to that count; a count over its ceiling
/// refuses, since a ceiling never rises.
pub fn lower(c: &Corpus) -> Result<()> {
    let path = c.root.join(FILE);
    let mut text = std::fs::read_to_string(&path)?;
    let mut over = Vec::new();
    for live in all(c) {
        match ceiling(c, &live.key) {
            Some(cap) if live.count > cap => over.push(format!("`{}` counts {} over {cap}", live.key, live.count)),
            Some(cap) if live.count < cap => text = set(&text, &live.key, live.count),
            Some(_) => {}
            None => over.push(format!("`{}` records no ceiling", live.key)),
        }
    }
    if !over.is_empty() {
        bail!("a ceiling never rises: {}", over.join("; "));
    }
    Ok(std::fs::write(path, text)?)
}

/// `text` with the line `name = <n>` under `[ceiling]` or `[ceiling.<table>]` set to `count`.
fn set(text: &str, key: &str, count: usize) -> String {
    let (section, name) = match key.split_once('.') {
        Some((t, n)) => (format!("[ceiling.{t}]"), n),
        None => ("[ceiling]".to_string(), key),
    };
    let mut inside = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('[') {
            inside = line.trim() == section;
        }
        let quoted = format!("\"{name}\"");
        let matches = line.split_once('=').map(|(k, _)| k.trim() == name || k.trim() == quoted).unwrap_or(false);
        if inside && matches {
            let k = line.split_once('=').unwrap().0.trim_end();
            out.push(format!("{k} = {count}"));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n") + "\n"
}
