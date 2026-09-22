//! Loading: the registry, the contract files, the records and the plan, parsed
//! into the shapes every check reads.

use crate::util::*;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static CLAUSE_ITEM: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^- `([^`]+)` — (.+)$").unwrap());
static CLAUSE_WHY: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^  \*(.+)\*$").unwrap());

// ---------------------------------------------------------------- registry

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ContractEntry {
    pub title: String,
    pub files: Vec<String>,
    pub titles: Vec<String>,
    pub gloss: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Named {
    #[serde(default)]
    pub gloss: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ErrorEntry {
    pub clause: String,
    #[serde(default)]
    pub gloss: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct LimitEntry {
    pub clause: String,
    pub value: toml::Value,
    pub unit: String,
    #[serde(default)]
    pub basis: String,
    #[serde(default)]
    pub gloss: String,
}

impl LimitEntry {
    pub fn value_text(&self) -> String {
        match &self.value {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            v => v.to_string(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Fragment {
    #[serde(default)]
    pub operation: BTreeMap<String, Named>,
    #[serde(default)]
    pub error: BTreeMap<String, ErrorEntry>,
    #[serde(default)]
    pub limit: BTreeMap<String, LimitEntry>,
}

#[derive(Deserialize, Default)]
struct ContractFile {
    #[serde(default)]
    contract: BTreeMap<String, ContractEntry>,
}

#[derive(Deserialize, Default)]
struct UnitFile {
    #[serde(default)]
    unit: BTreeMap<String, Named>,
}

#[derive(Serialize, Default)]
pub struct Registry {
    pub contracts: BTreeMap<String, ContractEntry>,
    pub units: BTreeMap<String, Named>,
    pub fragments: BTreeMap<String, Fragment>,
    #[serde(skip)]
    pub refused: HashMap<String, RefusedName>,
    #[serde(skip)]
    pub load_errors: Vec<(String, String)>,
}

impl Registry {
    pub fn load(root: &Path) -> Result<Registry> {
        let terms = root.join("spec/terms");
        let read = |name: &str| -> Result<String> {
            std::fs::read_to_string(terms.join(name)).with_context(|| format!("reading spec/terms/{name}"))
        };
        let contracts: ContractFile = toml::from_str(&read("contract.toml")?).context("contract.toml")?;
        let units: UnitFile = toml::from_str(&read("unit.toml")?).context("unit.toml")?;
        let refused: RefusedNamesFile =
            toml::from_str(&read("refused-names.toml").unwrap_or_default()).unwrap_or_default();
        let mut r = Registry {
            contracts: contracts.contract,
            units: units.unit,
            refused: refused.refused,
            ..Default::default()
        };
        let names: Vec<String> = r.contracts.keys().cloned().collect();
        for c in names {
            let rel = format!("spec/terms/{c}.toml");
            match std::fs::read_to_string(root.join(&rel)) {
                Ok(s) => match toml::from_str::<Fragment>(&s) {
                    Ok(f) => {
                        r.fragments.insert(c, f);
                    }
                    Err(e) => r.load_errors.push((rel, e.to_string().replace('\n', " "))),
                },
                Err(_) => r.load_errors.push((rel, "fragment missing".into())),
            }
        }
        Ok(r)
    }

    /// The canonical unit a token names, through its aliases.
    pub fn unit_of(&self, token: &str) -> Option<&str> {
        for (k, u) in &self.units {
            if k == token || u.aliases.iter().any(|a| a == token) {
                return Some(k.as_str());
            }
        }
        None
    }

    pub fn error_owner(&self, id: &str) -> Option<&ErrorEntry> {
        self.fragments.values().find_map(|f| f.error.get(id))
    }
}

// ---------------------------------------------------------------- documents

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Contract,
    Record,
    Plan,
    Guide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Blank,
    Front,
    Fence,
    Code,
    Heading,
    /// `` - `<subject>` — <statement> `` at operation level.
    ClauseItem,
    /// The two-space-indented `*…*` line following a clause item.
    ClauseWhy,
    /// A `` - ` `` line at operation level outside the clause-item shape.
    BadClause,
    TableRow,
    Unsettled,
    Prose,
}

#[derive(Serialize, Clone, Debug)]
pub struct Clause {
    pub id: String,
    pub contract: String,
    pub operation: String,
    pub subject: String,
    pub kind: String,
    pub statement: String,
    pub why: String,
    pub file: String,
    pub line: usize,
    /// Examples from the operation's `#### Scenarios` list: a WHEN/THEN sentence or a fixture path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<String>,
}

/// One item of a `#### Scenarios` list: the clause it names, its text, and where it sits.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub clause: String,
    pub operation: String,
    pub text: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug)]
pub struct Doc {
    pub rel: String,
    pub role: Role,
    pub lines: Vec<String>,
    pub kinds: Vec<LineKind>,
    pub contract: Option<String>,
    pub owns: Vec<String>,
    pub clauses: Vec<Clause>,
    /// Each `## <operation>` section's opening paragraph, by operation name.
    pub ledes: BTreeMap<String, String>,
}

impl Doc {
    /// Iterate (1-based line number, text, kind).
    pub fn each(&self) -> impl Iterator<Item = (usize, &str, LineKind)> {
        self.lines.iter().enumerate().map(|(i, l)| (i + 1, l.as_str(), self.kinds[i]))
    }
}

/// Split a markdown table row into trimmed cells, honouring `\|` and backticks.
pub fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut tick = false;
    let mut prev = '\0';
    for c in t.chars() {
        if c == '`' {
            tick = !tick;
        }
        if c == '|' && !tick && prev != '\\' {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
        prev = c;
    }
    out.push(cur.trim().to_string());
    out
}

pub fn is_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn parse(rel: &str, role: Role, text: &str) -> Doc {
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut kinds = vec![LineKind::Prose; lines.len()];
    let mut contract = None;
    let mut owns = Vec::new();
    let mut clauses = Vec::new();
    let mut i = 0;
    if lines.first().map(|l| l.trim() == "---").unwrap_or(false) {
        kinds[0] = LineKind::Front;
        i = 1;
        let mut in_owns = false;
        while i < lines.len() {
            kinds[i] = LineKind::Front;
            let l = lines[i].trim_end();
            if l.trim() == "---" {
                i += 1;
                break;
            }
            if let Some(v) = l.strip_prefix("contract:") {
                contract = Some(v.trim().to_string());
                in_owns = false;
            } else if l.starts_with("owns:") {
                in_owns = true;
            } else if in_owns {
                if let Some(v) = l.trim().strip_prefix("- ") {
                    owns.push(v.trim().to_string());
                }
            }
            i += 1;
        }
    }
    let mut fence = false;
    // the `## ` section in force, and whether a `###`+ heading sits between it and the line
    let mut section: Option<String> = None;
    let mut sub = false;
    let mut ledes: BTreeMap<String, String> = BTreeMap::new();
    // 0: no lede pending, 1: heading seen, 2: inside the lede paragraph
    let mut lede = 0u8;
    let at_operation = |section: &Option<String>, sub: bool| {
        role == Role::Contract && !sub && section.as_deref().map(|s| s != "Shapes").unwrap_or(false)
    };
    while i < lines.len() {
        let l = lines[i].as_str();
        let t = l.trim();
        let prev = if i > 0 { kinds[i - 1] } else { LineKind::Blank };
        if t.starts_with("```") {
            kinds[i] = LineKind::Fence;
            fence = !fence;
        } else if fence {
            kinds[i] = LineKind::Code;
        } else if t.is_empty() {
            kinds[i] = LineKind::Blank;
        } else if t.starts_with('#') {
            kinds[i] = LineKind::Heading;
            if let Some(h) = l.strip_prefix("## ") {
                section = Some(h.trim().to_string());
                sub = false;
            } else if l.starts_with("# ") {
                section = None;
                sub = false;
            } else {
                sub = true;
            }
        } else if at_operation(&section, sub) && l.starts_with("- `") {
            match CLAUSE_ITEM.captures(l) {
                Some(m) => {
                    kinds[i] = LineKind::ClauseItem;
                    let c = contract.clone().unwrap_or_default();
                    let op = section.clone().unwrap_or_default();
                    let subject = m[1].to_string();
                    clauses.push(Clause {
                        id: format!("{c}.{op}.{subject}"),
                        contract: c,
                        operation: op,
                        subject,
                        kind: "behavior".into(),
                        statement: m[2].trim().to_string(),
                        why: String::new(),
                        file: rel.to_string(),
                        line: i + 1,
                        scenarios: Vec::new(),
                    });
                }
                None => kinds[i] = LineKind::BadClause,
            }
        } else if prev == LineKind::ClauseItem && CLAUSE_WHY.is_match(l) {
            kinds[i] = LineKind::ClauseWhy;
            if let Some(cl) = clauses.last_mut() {
                cl.why = CLAUSE_WHY.captures(l).unwrap()[1].trim().to_string();
            }
        } else if t.starts_with('|') {
            kinds[i] = LineKind::TableRow;
        } else if t.starts_with("unsettled:") {
            kinds[i] = LineKind::Unsettled;
        } else {
            kinds[i] = LineKind::Prose;
        }
        // an operation's lede is the first paragraph after its heading, when prose
        lede = match (lede, kinds[i]) {
            (_, LineKind::Heading) if l.starts_with("## ") && at_operation(&section, sub) => 1,
            (1, LineKind::Blank) => 1,
            (1 | 2, LineKind::Prose) => {
                let e = ledes.entry(section.clone().unwrap_or_default()).or_default();
                if !e.is_empty() {
                    e.push(' ');
                }
                e.push_str(t);
                2
            }
            _ => 0,
        };
        i += 1;
    }
    Doc { rel: rel.to_string(), role, lines, kinds, contract, owns, clauses, ledes }
}

// ---------------------------------------------------------------- corpus

pub struct Corpus {
    pub root: PathBuf,
    pub reg: Registry,
    pub docs: Vec<Doc>,
}

impl Corpus {
    pub fn load(root: &Path) -> Result<Corpus> {
        let reg = Registry::load(root)?;
        let mut docs = Vec::new();
        let spec = root.join("spec");
        let mut contract_files: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&spec)? {
            let p = e?.path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with(".md") && name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                contract_files.push(format!("spec/{name}"));
            }
        }
        contract_files.sort();
        for rel in contract_files {
            let text = std::fs::read_to_string(root.join(&rel))?;
            docs.push(parse(&rel, Role::Contract, &text));
        }
        let mut recs: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(spec.join("adr")) {
            for e in rd {
                let p = e?.path();
                if p.extension().map(|x| x == "md").unwrap_or(false) {
                    recs.push(format!("spec/adr/{}", p.file_name().unwrap().to_string_lossy()));
                }
            }
        }
        recs.sort();
        for rel in recs {
            let text = std::fs::read_to_string(root.join(&rel))?;
            docs.push(parse(&rel, Role::Record, &text));
        }
        let mut guides: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(spec.join("guide")) {
            for e in rd {
                let p = e?.path();
                if p.extension().map(|x| x == "md").unwrap_or(false) {
                    guides.push(format!("spec/guide/{}", p.file_name().unwrap().to_string_lossy()));
                }
            }
        }
        guides.sort();
        for rel in guides {
            let text = std::fs::read_to_string(root.join(&rel))?;
            docs.push(parse(&rel, Role::Guide, &text));
        }
        if let Ok(text) = std::fs::read_to_string(spec.join("roadmap.md")) {
            docs.push(parse("spec/roadmap.md", Role::Plan, &text));
        }
        let mut c = Corpus { root: root.to_path_buf(), reg, docs };
        c.compute_kinds();
        c.attach_scenarios();
        Ok(c)
    }

    /// Every `- \`<clause id>\`: <text>` item under a `#### Scenarios` heading, with the
    /// `## <operation>` section it sits in.
    pub fn scenarios(&self) -> Vec<Scenario> {
        let mut out = Vec::new();
        for d in self.contracts() {
            let (mut op, mut inside) = (String::new(), false);
            for (n, l, k) in d.each() {
                if k == LineKind::Heading {
                    if let Some(t) = l.strip_prefix("## ") {
                        op = t.trim().to_string();
                        inside = false;
                    } else {
                        inside = l.trim() == "#### Scenarios";
                    }
                    continue;
                }
                let Some(item) = l.strip_prefix("- `").filter(|_| inside) else { continue };
                let Some((id, text)) = item.split_once("`:") else { continue };
                out.push(Scenario {
                    clause: id.to_string(),
                    operation: op.clone(),
                    text: text.trim().to_string(),
                    file: d.rel.clone(),
                    line: n,
                });
            }
        }
        out
    }

    fn attach_scenarios(&mut self) {
        let found = self.scenarios();
        for d in self.docs.iter_mut() {
            for cl in d.clauses.iter_mut() {
                cl.scenarios = found.iter().filter(|s| s.clause == cl.id).map(|s| s.text.clone()).collect();
            }
        }
    }

    fn compute_kinds(&mut self) {
        let mut refusal = BTreeSet::new();
        let mut limit = BTreeSet::new();
        for f in self.reg.fragments.values() {
            for e in f.error.values() {
                refusal.insert(e.clause.clone());
            }
            for l in f.limit.values() {
                limit.insert(l.clause.clone());
            }
        }
        for d in &mut self.docs {
            for cl in &mut d.clauses {
                cl.kind = if refusal.contains(&cl.id) {
                    "refusal".into()
                } else if limit.contains(&cl.id) {
                    "limit".into()
                } else {
                    "behavior".into()
                };
            }
        }
    }

    pub fn contracts(&self) -> impl Iterator<Item = &Doc> {
        self.docs.iter().filter(|d| d.role == Role::Contract)
    }

    pub fn guides(&self) -> impl Iterator<Item = &Doc> {
        self.docs.iter().filter(|d| d.role == Role::Guide)
    }

    /// Every operation's lede, keyed `<contract>.<operation>`.
    pub fn ledes(&self) -> BTreeMap<String, &str> {
        self.contracts()
            .flat_map(|d| {
                let c = d.contract.clone().unwrap_or_default();
                d.ledes.iter().map(move |(op, t)| (format!("{c}.{op}"), t.as_str()))
            })
            .collect()
    }

    pub fn records(&self) -> impl Iterator<Item = &Doc> {
        self.docs.iter().filter(|d| d.role == Role::Record)
    }

    pub fn clauses(&self) -> impl Iterator<Item = &Clause> {
        self.contracts().flat_map(|d| d.clauses.iter())
    }

    pub fn clause_map(&self) -> BTreeMap<String, &Clause> {
        self.clauses().map(|c| (c.id.clone(), c)).collect()
    }
}

// ---------------------------------------------------------------- findings

#[derive(Serialize, Clone, Debug)]
pub struct Finding {
    pub check: String,
    pub file: String,
    pub line: usize,
    pub code: String,
    pub message: String,
}

impl Finding {
    pub fn new(check: &str, file: &str, line: usize, code: &str, message: String) -> Finding {
        Finding { check: check.into(), file: file.into(), line, code: code.into(), message }
    }
}

/// Words in a statement, a `{{id}}` pointer counting as one.
pub fn word_count(s: &str) -> usize {
    strip_pointers(s, " ref ").split_whitespace().count()
}

/// Replace every `{{...}}` span with `with`.
pub fn strip_pointers(s: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find("{{") {
        out.push_str(&rest[..a]);
        match rest[a..].find("}}") {
            Some(b) => {
                out.push_str(with);
                rest = &rest[a + b + 2..];
            }
            None => {
                rest = &rest[a..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Every `{{id}}` in a string.
pub fn pointers(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(a) = rest.find("{{") {
        match rest[a..].find("}}") {
            Some(b) => {
                out.push(rest[a + 2..a + b].trim().to_string());
                rest = &rest[a + b + 2..];
            }
            None => break,
        }
    }
    out
}
