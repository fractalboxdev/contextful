//! Loading the corpus: the scope table, the registries, and every authored file
//! parsed into clause rows, prose lines and unsettled lines.
//!
//! Implements `corpus.registry.invariant.scan-boundary-is-data` — every path set a
//! check reads comes from `spec/terms/scope.toml`, so nothing below hard-codes a path.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- glob

/// Minimal glob: `**` spans directory separators, `*` does not.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn m(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p.starts_with(b"**") {
            let rest = &p[2..];
            let rest = if rest.starts_with(b"/") { &rest[1..] } else { rest };
            if m(rest, s) {
                return true;
            }
            for i in 0..s.len() {
                if m(rest, &s[i + 1..]) {
                    return true;
                }
            }
            return false;
        }
        match p[0] {
            b'*' => {
                let mut i = 0;
                loop {
                    if m(&p[1..], &s[i..]) {
                        return true;
                    }
                    if i >= s.len() || s[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            c => !s.is_empty() && s[0] == c && m(&p[1..], &s[1..]),
        }
    }
    m(pattern.as_bytes(), path.as_bytes())
}

// ---------------------------------------------------------------- scope

#[derive(Debug, Default)]
pub struct Scope {
    pub roles: BTreeMap<String, Vec<String>>,
    pub authored_include: Vec<String>,
    pub authored_exclude: Vec<String>,
    pub checks: BTreeMap<String, toml::Value>,
}

impl Scope {
    pub fn load(root: &Path) -> Result<Scope> {
        let raw = std::fs::read_to_string(root.join("spec/terms/scope.toml"))
            .context("reading spec/terms/scope.toml")?;
        let v: toml::Value = raw.parse()?;
        let mut s = Scope::default();
        if let Some(t) = v.get("role").and_then(|x| x.as_table()) {
            for (k, val) in t {
                s.roles.insert(k.clone(), str_list(val));
            }
        }
        if let Some(t) = v.get("authored").and_then(|x| x.as_table()) {
            s.authored_include = t.get("include").map(str_list).unwrap_or_default();
            s.authored_exclude = t.get("exclude").map(str_list).unwrap_or_default();
        }
        if let Some(t) = v.as_table() {
            for (k, val) in t {
                if let Some(name) = k.strip_prefix("check.") {
                    s.checks.insert(name.to_string(), val.clone());
                }
            }
        }
        // toml parses `[check.ids]` as nested tables under `check`.
        if let Some(t) = v.get("check").and_then(|x| x.as_table()) {
            for (k, val) in t {
                s.checks.insert(k.clone(), val.clone());
            }
        }
        Ok(s)
    }

    /// The most specific matching glob wins, so `spec/roadmap.md` reads as `plan`
    /// rather than as the `contract` role `spec/*.md` would also match.
    pub fn role_of(&self, rel: &str) -> Option<String> {
        let mut best: Option<(usize, String)> = None;
        for (role, globs) in &self.roles {
            for g in globs {
                if glob_match(g, rel) {
                    let score = g.chars().filter(|c| *c != '*').count();
                    if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                        best = Some((score, role.clone()));
                    }
                }
            }
        }
        best.map(|(_, r)| r)
    }

    pub fn is_authored(&self, rel: &str) -> bool {
        let inc = self.authored_include.iter().any(|g| glob_match(g, rel));
        let exc = self.authored_exclude.iter().any(|g| glob_match(g, rel));
        inc && !exc
    }

    /// The roles a check reads, from its `[check.<name>] reads` list.
    pub fn reads(&self, check: &str) -> Vec<String> {
        self.checks
            .get(check)
            .and_then(|v| v.get("reads"))
            .map(str_list)
            .unwrap_or_default()
    }

    pub fn check_int(&self, check: &str, key: &str) -> Option<i64> {
        self.checks
            .get(check)
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_integer())
    }

    pub fn exempt_roles(&self, check: &str) -> Vec<String> {
        self.checks
            .get(check)
            .and_then(|v| v.get("exempt-roles"))
            .map(str_list)
            .unwrap_or_default()
    }
}

fn str_list(v: &toml::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- registries

#[derive(Debug, Clone, Serialize)]
pub struct ContractEntry {
    pub name: String,
    pub title: String,
    pub files: Vec<String>,
    pub titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationEntry {
    pub key: String,
    pub contract: String,
    pub operation: String,
    pub file: Option<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TermEntry {
    pub name: String,
    pub owner: String,
    pub resolves: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEntry {
    pub name: String,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitEntry {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnitEntry {
    pub name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Registry {
    pub contracts: BTreeMap<String, ContractEntry>,
    pub operations: BTreeMap<String, OperationEntry>,
    pub terms: BTreeMap<String, TermEntry>,
    pub errors: BTreeMap<String, ErrorEntry>,
    pub limits: BTreeMap<String, LimitEntry>,
    pub units: BTreeMap<String, UnitEntry>,
    pub retired_ids: BTreeSet<String>,
}

fn table_of(v: &toml::Value, key: &str) -> BTreeMap<String, toml::Value> {
    v.get(key)
        .and_then(|x| x.as_table())
        .map(|t| t.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

impl Registry {
    pub fn load(root: &Path) -> Result<Registry> {
        let mut r = Registry::default();
        let terms_dir = root.join("spec/terms");

        let contract: toml::Value = std::fs::read_to_string(terms_dir.join("contract.toml"))?.parse()?;
        for (name, v) in table_of(&contract, "contract") {
            let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let files = v.get("files").map(str_list).unwrap_or_default();
            let mut titles = v.get("titles").map(str_list).unwrap_or_default();
            if titles.is_empty() {
                titles = vec![title.clone(); files.len()];
            }
            r.contracts.insert(
                name.clone(),
                ContractEntry { name, title, files, titles },
            );
        }

        let ops: toml::Value = std::fs::read_to_string(terms_dir.join("operation.toml"))?.parse()?;
        for (key, v) in table_of(&ops, "operation") {
            let (contract, operation) = match key.split_once('.') {
                Some((a, b)) => (a.to_string(), b.to_string()),
                None => (String::new(), key.clone()),
            };
            r.operations.insert(
                key.clone(),
                OperationEntry {
                    key,
                    contract,
                    operation,
                    file: v.get("file").and_then(|x| x.as_str()).map(|s| s.to_string()),
                    aliases: v.get("aliases").map(str_list).unwrap_or_default(),
                },
            );
        }

        let terms: toml::Value = std::fs::read_to_string(terms_dir.join("term.toml"))?.parse()?;
        for (name, v) in table_of(&terms, "term") {
            r.terms.insert(
                name.clone(),
                TermEntry {
                    name,
                    owner: v.get("owner").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    resolves: v.get("resolves").and_then(|x| x.as_str()).unwrap_or("concept").to_string(),
                    aliases: v.get("aliases").map(str_list).unwrap_or_default(),
                },
            );
        }
        for (id, v) in table_of(&terms, "retired") {
            let _ = v;
            r.retired_ids.insert(id);
        }

        let errs: toml::Value = std::fs::read_to_string(terms_dir.join("error.toml"))?.parse()?;
        for (name, v) in table_of(&errs, "error") {
            r.errors.insert(
                name.clone(),
                ErrorEntry {
                    name,
                    owner: v.get("owner").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                },
            );
        }

        let lims: toml::Value = std::fs::read_to_string(terms_dir.join("limit.toml"))?.parse()?;
        for (name, v) in table_of(&lims, "limit") {
            let value = v
                .get("value")
                .map(|x| match x {
                    toml::Value::Integer(i) => *i as f64,
                    toml::Value::Float(f) => *f,
                    _ => f64::NAN,
                })
                .unwrap_or(f64::NAN);
            r.limits.insert(
                name.clone(),
                LimitEntry {
                    name,
                    value,
                    unit: v.get("unit").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    owner: v.get("owner").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                },
            );
        }

        let units: toml::Value = std::fs::read_to_string(terms_dir.join("unit.toml"))?.parse()?;
        for (name, v) in table_of(&units, "unit") {
            r.units.insert(
                name.clone(),
                UnitEntry {
                    name,
                    aliases: v.get("aliases").map(str_list).unwrap_or_default(),
                },
            );
        }

        Ok(r)
    }

    /// Canonical unit name for a token, matching a canonical spelling or a
    /// registered alias, case-insensitively.
    pub fn unit_of(&self, token: &str) -> Option<&str> {
        let lower = token.to_ascii_lowercase();
        for (name, u) in &self.units {
            if name.to_ascii_lowercase() == lower {
                return Some(name);
            }
            if u.aliases.iter().any(|a| a.to_ascii_lowercase() == lower) {
                return Some(name);
            }
        }
        None
    }
}

// ---------------------------------------------------------------- documents

#[derive(Debug, Clone, Serialize)]
pub struct Clause {
    pub id: String,
    pub contract: String,
    pub operation: String,
    pub kind: String,
    pub subject: String,
    pub statement: String,
    pub decided_by: String,
    pub file: String,
    pub line: usize,
    /// The `## Clauses — <operation>` heading this row sits under.
    pub section: String,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub no: usize,
    pub raw: String,
    /// `raw` with front matter dropped, fenced code blanked and every `{{...}}`
    /// span blanked — the text every vocabulary check reads.
    pub authored: String,
    pub in_fence: bool,
    pub fence_info: String,
    pub is_front_matter: bool,
    pub is_clause_row: bool,
    pub is_table_row: bool,
    pub is_unsettled: bool,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub rel: String,
    pub role: String,
    pub contract: Option<String>,
    pub owns: Vec<String>,
    pub lines: Vec<Line>,
    pub headings: Vec<(usize, usize, String)>,
    pub clauses: Vec<Clause>,
    pub unsettled: Vec<(usize, String)>,
    pub line_count: usize,
}

/// Split a markdown table row on `|`, leaving pipes inside backtick spans alone.
fn split_row(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut tick = false;
    for c in s.chars() {
        match c {
            '`' => {
                tick = !tick;
                cur.push(c);
            }
            '|' if !tick => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    if out.first().map(|s| s.is_empty()).unwrap_or(false) {
        out.remove(0);
    }
    if out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

fn strip_ticks(s: &str) -> String {
    s.trim().trim_matches('`').trim().to_string()
}

pub fn blank_transclusions(s: &str) -> String {
    // `corpus.registry.invariant.scan-boundary-is-data`: every span a reference
    // produced is out of authored scope.
    let mut out = String::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if i + 1 < b.len() && b[i] == '{' && b[i + 1] == '{' {
            let mut j = i + 2;
            while j + 1 < b.len() && !(b[j] == '}' && b[j + 1] == '}') {
                j += 1;
            }
            i = (j + 2).min(b.len());
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

pub fn load_docs(root: &Path, scope: &Scope) -> Result<Vec<Doc>> {
    let mut docs = Vec::new();
    for entry in walkdir::WalkDir::new(root.join("spec"))
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let role = match scope.role_of(&rel) {
            Some(r) => r,
            None => continue,
        };
        if !rel.ends_with(".md") && !rel.ends_with(".toml") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())?;
        docs.push(parse_doc(rel, role, &text));
    }
    docs.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(docs)
}

fn parse_doc(rel: String, role: String, text: &str) -> Doc {
    let raw_lines: Vec<&str> = text.lines().collect();
    let mut contract = None;
    let mut owns = Vec::new();
    let mut fm_end = 0usize;
    if raw_lines.first().map(|l| l.trim() == "---").unwrap_or(false) {
        let mut in_owns = false;
        for (i, l) in raw_lines.iter().enumerate().skip(1) {
            if l.trim() == "---" {
                fm_end = i + 1;
                break;
            }
            if let Some(v) = l.strip_prefix("contract:") {
                contract = Some(v.trim().to_string());
                in_owns = false;
            } else if l.trim_end() == "owns:" {
                in_owns = true;
            } else if in_owns {
                if let Some(v) = l.trim().strip_prefix("- ") {
                    owns.push(v.trim().to_string());
                } else if !l.trim().is_empty() {
                    in_owns = false;
                }
            }
        }
    }

    let mut lines = Vec::with_capacity(raw_lines.len());
    let mut headings = Vec::new();
    let mut clauses = Vec::new();
    let mut unsettled = Vec::new();
    let mut in_fence = false;
    let mut fence_info = String::new();
    let mut section = String::new();

    for (i, raw) in raw_lines.iter().enumerate() {
        let no = i + 1;
        let is_fm = i < fm_end;
        let trimmed = raw.trim_start();
        let fence_open = trimmed.starts_with("```");
        let mut this_in_fence = in_fence;
        if fence_open {
            if in_fence {
                in_fence = false;
                this_in_fence = true;
            } else {
                in_fence = true;
                this_in_fence = true;
                fence_info = trimmed.trim_start_matches('`').trim().to_string();
            }
        }
        let cur_info = if this_in_fence { fence_info.clone() } else { String::new() };
        if fence_open && !in_fence {
            fence_info.clear();
        }

        let mut is_clause_row = false;
        let is_table_row = !this_in_fence && !is_fm && trimmed.starts_with('|');
        let is_unsettled = !this_in_fence && !is_fm && trimmed.starts_with("unsettled:");

        if !this_in_fence && !is_fm {
            if let Some(rest) = raw.strip_prefix("## ") {
                headings.push((no, 2, rest.trim().to_string()));
                section = rest
                    .trim()
                    .strip_prefix("Clauses")
                    .map(|s| s.trim_start_matches([' ', '—', '-']).trim().to_string())
                    .unwrap_or_default();
            } else if let Some(rest) = raw.strip_prefix("# ") {
                headings.push((no, 1, rest.trim().to_string()));
            } else if let Some(rest) = raw.strip_prefix("### ") {
                headings.push((no, 3, rest.trim().to_string()));
            }
            if is_unsettled {
                unsettled.push((no, raw.trim().to_string()));
            }
            if is_table_row {
                let cells = split_row(raw.trim());
                if cells.len() >= 2 {
                    let first = strip_ticks(&cells[0]);
                    let looks_like_id = first.split('.').count() == 4
                        && cells[0].trim().starts_with('`')
                        && !first.contains(' ');
                    if looks_like_id {
                        is_clause_row = true;
                        let segs: Vec<&str> = first.split('.').collect();
                        clauses.push(Clause {
                            id: first.clone(),
                            contract: segs[0].to_string(),
                            operation: segs[1].to_string(),
                            kind: segs[2].to_string(),
                            subject: segs[3].to_string(),
                            statement: cells.get(1).cloned().unwrap_or_default(),
                            decided_by: strip_ticks(cells.get(2).map(|s| s.as_str()).unwrap_or("")),
                            file: rel.clone(),
                            line: no,
                            section: section.clone(),
                        });
                    }
                }
            }
        }

        let authored = if is_fm || this_in_fence {
            String::new()
        } else {
            blank_transclusions(raw)
        };

        lines.push(Line {
            no,
            raw: raw.to_string(),
            authored,
            in_fence: this_in_fence,
            fence_info: cur_info,
            is_front_matter: is_fm,
            is_clause_row,
            is_table_row,
            is_unsettled,
        });
    }

    Doc {
        rel,
        role,
        contract,
        owns,
        line_count: raw_lines.len(),
        lines,
        headings,
        clauses,
        unsettled,
    }
}

// ---------------------------------------------------------------- findings

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub check: String,
    pub file: String,
    pub line: usize,
    pub code: String,
    pub message: String,
}

impl Finding {
    pub fn new(check: &str, file: &str, line: usize, code: &str, message: String) -> Finding {
        Finding {
            check: check.to_string(),
            file: file.to_string(),
            line,
            code: code.to_string(),
            message,
        }
    }
}

pub struct Corpus {
    pub root: PathBuf,
    pub scope: Scope,
    pub reg: Registry,
    pub docs: Vec<Doc>,
}

impl Corpus {
    pub fn load(root: &Path) -> Result<Corpus> {
        let scope = Scope::load(root)?;
        let reg = Registry::load(root)?;
        let docs = load_docs(root, &scope)?;
        Ok(Corpus {
            root: root.to_path_buf(),
            scope,
            reg,
            docs,
        })
    }

    pub fn docs_for(&self, check: &str) -> Vec<&Doc> {
        let roles = self.scope.reads(check);
        let exempt = self.scope.exempt_roles(check);
        self.docs
            .iter()
            .filter(|d| {
                if exempt.contains(&d.role) {
                    return false;
                }
                if roles.iter().any(|r| r == "authored") && self.scope.is_authored(&d.rel) {
                    return true;
                }
                roles.contains(&d.role)
            })
            .collect()
    }

    pub fn all_clauses(&self) -> Vec<&Clause> {
        self.docs.iter().flat_map(|d| d.clauses.iter()).collect()
    }

    pub fn clause_ids(&self) -> BTreeSet<String> {
        self.all_clauses().iter().map(|c| c.id.clone()).collect()
    }

    pub fn contract_of_path(&self, rel: &str) -> Option<&ContractEntry> {
        self.reg.contracts.values().find(|c| c.files.iter().any(|f| f == rel))
    }

    pub fn records(&self) -> Vec<&Doc> {
        self.docs.iter().filter(|d| d.role == "record").collect()
    }
}

/// Every backticked span in a line, as (byte offset of the content, content).
pub fn tick_spans(s: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j] != b'`' {
                j += 1;
            }
            if j < b.len() {
                out.push((start, s[start..j].to_string()));
                i = j + 1;
                continue;
            } else {
                break;
            }
        }
        i += 1;
    }
    out
}

/// A line with every backticked span blanked, so a prose scan never reads code.
pub fn without_ticks(s: &str) -> String {
    let mut out = String::new();
    let mut tick = false;
    for c in s.chars() {
        if c == '`' {
            tick = !tick;
            out.push(' ');
            continue;
        }
        out.push(if tick { ' ' } else { c });
    }
    out
}

pub type Index = HashMap<String, Vec<String>>;
