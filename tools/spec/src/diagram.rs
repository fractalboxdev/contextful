//! `corpus.diagram`: every `mermaid` fence parsed into a small model — a flowchart's
//! nodes, edges and subgraphs, a sequence's participants and messages, a state
//! machine's transitions — and one function per clause over that model.

use crate::corpus::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

// ---------------------------------------------------------------- model

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Box,
    Round,
    Stadium,
    Cylinder,
    Circle,
    Decision,
    Hexagon,
    Subroutine,
    Asymmetric,
    Slanted,
}

#[derive(Debug)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub shape: Shape,
    pub line: usize,
    pub parent: Option<String>,
}

#[derive(Debug)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub two_way: bool,
    pub line: usize,
}

#[derive(Debug)]
pub struct Subgraph {
    pub id: String,
    pub parent: Option<String>,
    pub depth: usize,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Flowchart {
    pub header_line: usize,
    pub direction: Option<String>,
    pub direction_lines: Vec<usize>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub subgraphs: Vec<Subgraph>,
}

#[derive(Debug, Default)]
pub struct Sequence {
    pub header_line: usize,
    /// Declared participant ids, with the line declaring each.
    pub declared: Vec<(String, usize)>,
    /// `(from, to, label, line)` per message.
    pub messages: Vec<(String, String, String, usize)>,
    pub notes: Vec<(String, usize)>,
    pub max_fragment_depth: (usize, usize),
}

#[derive(Debug, Default)]
pub struct StateMachine {
    pub header_line: usize,
    /// Top-level `[*] -->` lines.
    pub starts: Vec<usize>,
    /// `(from, to, label, line)`; a composite's inner `[*]` start is an edge from the composite.
    pub transitions: Vec<(String, String, String, usize)>,
    /// Every state, with the line first naming it.
    pub states: Vec<(String, usize)>,
    /// States with an outgoing transition, `[*]` counting as a target.
    pub leaving: BTreeSet<String>,
}

pub enum Model {
    Flowchart(Flowchart),
    Sequence(Sequence),
    State(StateMachine),
    Other,
}

/// One fenced `mermaid` block of one document.
pub struct Fence<'a> {
    pub doc: &'a Doc,
    pub model: Model,
}

pub fn fences(c: &Corpus) -> Vec<Fence<'_>> {
    let mut out = Vec::new();
    for d in &c.docs {
        let mut body: Option<Vec<(usize, &str)>> = None;
        for (n, l, k) in d.each() {
            match (k, body.as_mut()) {
                (LineKind::Fence, None) if l.trim().starts_with("```mermaid") => body = Some(Vec::new()),
                (LineKind::Fence, Some(_)) => out.push(Fence { doc: d, model: parse(&body.take().unwrap()) }),
                (LineKind::Code, Some(b)) => b.push((n, l)),
                _ => {}
            }
        }
    }
    out
}

pub fn parse(lines: &[(usize, &str)]) -> Model {
    let Some(&(first, header)) = lines.iter().find(|(_, l)| !l.trim().is_empty() && !l.trim().starts_with("%%")) else {
        return Model::Other;
    };
    let head = header.split_whitespace().next().unwrap_or("");
    let rest: Vec<(usize, &str)> = lines.iter().copied().filter(|(n, _)| *n > first).collect();
    match head {
        "flowchart" | "graph" => Model::Flowchart(flowchart(first, header, &rest)),
        "sequenceDiagram" => Model::Sequence(sequence(first, &rest)),
        "stateDiagram" | "stateDiagram-v2" => Model::State(state_machine(first, &rest)),
        _ => Model::Other,
    }
}

// ---------------------------------------------------------------- flowchart parsing

static NODE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]+").unwrap());
static LINK_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(<)?(?:--|==|-\.)\s+("[^"]*"|[^"]*?)\s+(?:-{2,}[>ox]?|={2,}[>ox]?|\.-+[>ox]?)"#).unwrap()
});
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(<)?(?:-{2,}|={2,}|-\.+-|~~~)[>ox]?").unwrap());
static PIPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\|([^|]*)\|").unwrap());
static CLASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^:::[A-Za-z0-9_-]+").unwrap());
static SUBGRAPH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^subgraph\s+([A-Za-z0-9_]+)\s*(\[.*)?$").unwrap());

const OPENERS: [(&str, Shape, &[&str]); 11] = [
    ("([", Shape::Stadium, &["])"]),
    ("[[", Shape::Subroutine, &["]]"]),
    ("[(", Shape::Cylinder, &[")]"]),
    ("((", Shape::Circle, &["))"]),
    ("{{", Shape::Hexagon, &["}}"]),
    ("[/", Shape::Slanted, &["/]", "\\]"]),
    ("[\\", Shape::Slanted, &["\\]", "/]"]),
    ("[", Shape::Box, &["]"]),
    ("(", Shape::Round, &[")"]),
    ("{", Shape::Decision, &["}"]),
    (">", Shape::Asymmetric, &["]"]),
];

fn unquote(s: &str) -> String {
    let t = s.trim();
    t.strip_prefix('"').and_then(|u| u.strip_suffix('"')).unwrap_or(t).trim().to_string()
}

struct Builder<'a> {
    f: &'a mut Flowchart,
    stack: Vec<String>,
}

impl Builder<'_> {
    fn mention(&mut self, id: &str, shape: Option<(Shape, String)>, line: usize) {
        match self.f.nodes.iter_mut().find(|n| n.id == id) {
            Some(n) => {
                if let Some((s, l)) = shape {
                    n.shape = s;
                    n.label = l;
                }
            }
            None => {
                let (shape, label) = shape.unwrap_or((Shape::Box, id.to_string()));
                self.f.nodes.push(Node { id: id.into(), label, shape, line, parent: self.stack.last().cloned() });
            }
        }
    }

    /// One node reference at the head of `s`: its id, shape and label, and the bytes consumed.
    fn node_ref(s: &str) -> Option<(String, Option<(Shape, String)>, usize)> {
        let lead = s.len() - s.trim_start().len();
        let t = &s[lead..];
        let id = NODE_ID.find(t)?.as_str().to_string();
        let mut at = lead + id.len();
        let mut shape = None;
        if let Some((open, kind, closers)) = OPENERS.iter().find(|(o, _, _)| s[at..].starts_with(o)) {
            let body = &s[at + open.len()..];
            let (label, used) = match body.strip_prefix('"') {
                Some(q) => {
                    let end = q.find('"').unwrap_or(q.len());
                    let after = &q[(end + 1).min(q.len())..];
                    let close = closers.iter().filter_map(|c| after.find(c).map(|i| i + c.len())).min().unwrap_or(0);
                    (q[..end].to_string(), 1 + end + 1 + close)
                }
                None => {
                    let (i, c) = closers.iter().filter_map(|c| body.find(c).map(|i| (i, c.len()))).min().unwrap_or((body.len(), 0));
                    (body[..i].to_string(), i + c)
                }
            };
            shape = Some((*kind, label.trim().to_string()));
            at += open.len() + used.min(body.len());
        }
        if let Some(m) = CLASS.find(&s[at..]) {
            at += m.end();
        }
        Some((id, shape, at))
    }

    /// One statement: node groups joined by links, `&` fanning a group.
    fn statement(&mut self, s: &str, line: usize) {
        let mut at = 0;
        let mut previous: Vec<String> = Vec::new();
        let mut pending: Option<(Option<String>, bool)> = None;
        loop {
            let mut group = Vec::new();
            while let Some((id, shape, used)) = Self::node_ref(&s[at..]) {
                self.mention(&id, shape, line);
                group.push(id);
                at += used;
                let rest = s[at..].trim_start();
                if let Some(r) = rest.strip_prefix('&') {
                    at = s.len() - r.len();
                } else {
                    break;
                }
            }
            if group.is_empty() {
                return;
            }
            if let Some((label, two_way)) = pending.take() {
                for from in &previous {
                    for to in &group {
                        self.f.edges.push(Edge { from: from.clone(), to: to.clone(), label: label.clone(), two_way, line });
                    }
                }
            }
            let rest = &s[at..];
            let lead = rest.len() - rest.trim_start().len();
            let t = &rest[lead..];
            let (mut label, two_way, used) = if let Some(m) = LINK_TEXT.captures(t) {
                (Some(unquote(&m[2])), m.get(1).is_some(), m.get(0).unwrap().end())
            } else if let Some(m) = LINK.captures(t) {
                (None, m.get(1).is_some(), m.get(0).unwrap().end())
            } else {
                return;
            };
            at += lead + used;
            if let Some(p) = PIPE.captures(&s[at..]) {
                label = Some(unquote(&p[1]));
                at += p.get(0).unwrap().end();
            }
            pending = Some((label.filter(|l| !l.is_empty()), two_way));
            previous = group;
        }
    }
}

fn flowchart(first: usize, header: &str, lines: &[(usize, &str)]) -> Flowchart {
    let mut f = Flowchart { header_line: first, direction: header.split_whitespace().nth(1).map(str::to_string), ..Default::default() };
    let mut b = Builder { f: &mut f, stack: Vec::new() };
    for &(n, l) in lines {
        let t = l.trim();
        let head = t.split_whitespace().next().unwrap_or("");
        match head {
            "" => {}
            _ if t.starts_with("%%") => {}
            "subgraph" => {
                let id = SUBGRAPH.captures(t).map(|m| m[1].to_string()).unwrap_or_else(|| t["subgraph".len()..].trim().to_string());
                let depth = b.stack.len() + 1;
                b.f.subgraphs.push(Subgraph { id: id.clone(), parent: b.stack.last().cloned(), depth, line: n });
                b.stack.push(id);
            }
            "end" => {
                b.stack.pop();
            }
            "direction" => b.f.direction_lines.push(n),
            "classDef" | "class" | "style" | "linkStyle" | "click" => {}
            _ => b.statement(t, n),
        }
    }
    let subgraphs: BTreeSet<String> = f.subgraphs.iter().map(|s| s.id.clone()).collect();
    f.nodes.retain(|n| !subgraphs.contains(&n.id));
    f
}

// ---------------------------------------------------------------- sequence and state parsing

static PARTICIPANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:create\s+)?(?:participant|actor)\s+([A-Za-z0-9_]+)").unwrap());
static MESSAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+)\s*(?:--?>>|--?>|--?x|--?\))\s*[+-]?\s*([A-Za-z0-9_]+)\s*:\s*(.*)$").unwrap());
static NOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^note\s+(?:over|left of|right of)\s+[^:]+:\s*(.*)$").unwrap());

fn sequence(first: usize, lines: &[(usize, &str)]) -> Sequence {
    let mut s = Sequence { header_line: first, ..Default::default() };
    let mut stack: Vec<bool> = Vec::new();
    for &(n, l) in lines {
        let t = l.trim();
        let head = t.split_whitespace().next().unwrap_or("");
        if let Some(m) = PARTICIPANT.captures(t) {
            s.declared.push((m[1].to_string(), n));
        } else if let Some(m) = MESSAGE.captures(t) {
            s.messages.push((m[1].to_string(), m[2].to_string(), m[3].trim().to_string(), n));
        } else if let Some(m) = NOTE.captures(t) {
            s.notes.push((m[1].trim().to_string(), n));
        } else if matches!(head, "alt" | "opt" | "loop" | "par" | "critical" | "break") {
            stack.push(true);
            let depth = stack.iter().filter(|x| **x).count();
            if depth > s.max_fragment_depth.0 {
                s.max_fragment_depth = (depth, n);
            }
        } else if matches!(head, "box" | "rect") {
            stack.push(false);
        } else if head == "end" {
            stack.pop();
        }
    }
    s
}

static TRANSITION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+)\s*-->\s*([^\s:]+)\s*(?::\s*(.*))?$").unwrap());
static COMPOSITE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^state\s+(?:"[^"]*"\s+as\s+)?([A-Za-z0-9_]+)\s*\{$"#).unwrap());

fn state_machine(first: usize, lines: &[(usize, &str)]) -> StateMachine {
    let mut m = StateMachine { header_line: first, ..Default::default() };
    let mut stack: Vec<String> = Vec::new();
    let mut in_note = false;
    let name = |m: &mut StateMachine, s: &str, n: usize| {
        if s != "[*]" && !m.states.iter().any(|(x, _)| x == s) {
            m.states.push((s.to_string(), n));
        }
    };
    for &(n, l) in lines {
        let t = l.trim();
        if in_note {
            in_note = !t.starts_with("end note");
            continue;
        }
        if t.starts_with("note ") {
            in_note = !t.contains(':');
            continue;
        }
        if let Some(c) = COMPOSITE.captures(t) {
            name(&mut m, &c[1], n);
            stack.push(c[1].to_string());
        } else if t == "}" {
            stack.pop();
        } else if let Some(c) = TRANSITION.captures(t) {
            let (from, to, label) = (&c[1], &c[2], c.get(3).map_or("", |x| x.as_str()).trim().to_string());
            name(&mut m, from, n);
            name(&mut m, to, n);
            match (from == "[*]", stack.last()) {
                (true, None) => m.starts.push(n),
                (true, Some(parent)) => m.transitions.push((parent.clone(), to.to_string(), label, n)),
                (false, _) => {
                    m.leaving.insert(from.to_string());
                    if to != "[*]" {
                        m.transitions.push((from.to_string(), to.to_string(), label, n));
                    }
                }
            }
            if from == "[*]" {
                if let Some(parent) = stack.last() {
                    m.leaving.insert(parent.clone());
                }
            }
        }
    }
    // the top-level start's targets
    for &(n, l) in lines {
        if let Some(c) = TRANSITION.captures(l.trim()) {
            if &c[1] == "[*]" && m.starts.contains(&n) {
                m.transitions.push(("[*]".into(), c[2].to_string(), String::new(), n));
            }
        }
    }
    m
}

// ---------------------------------------------------------------- rules

/// Every named bound the diagram clauses read from `spec/terms/corpus.toml`.
const BOUNDS: [&str; 13] = [
    "corpus-node-words",
    "corpus-edge-words",
    "corpus-decision-words",
    "corpus-subgraph-depth",
    "corpus-chart-nodes",
    "corpus-chart-edges",
    "corpus-participants",
    "corpus-messages",
    "corpus-self-messages",
    "corpus-fragment-depth",
    "corpus-message-words",
    "corpus-note-words",
    "corpus-transition-words",
];

static BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").unwrap());
static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#?[A-Za-z0-9]+;").unwrap());

/// A label as a reader sees it: line breaks as spaces, markup and quotes removed.
fn plain(label: &str) -> String {
    let text = BREAK.replace_all(label, " ");
    ENTITY.replace_all(&text, "").replace(['`', '*', '"'], "").trim().to_string()
}

fn words(label: &str) -> usize {
    plain(label).split_whitespace().count()
}

fn error_in(c: &Corpus, label: &str) -> Option<String> {
    label.split(|ch: char| !ch.is_alphanumeric() && ch != '_').find(|w| c.reg.error_owner(w).is_some()).map(str::to_string)
}

fn first_word(label: &str) -> String {
    plain(label).split_whitespace().next().unwrap_or("").trim_matches(|ch: char| !ch.is_alphanumeric()).to_lowercase()
}

const ARTICLES: [&str; 7] = ["a", "an", "the", "each", "every", "its", "their"];
const STORAGE: [&str; 8] = ["table", "tables", "store", "stores", "log", "logs", "queue", "queues"];
const CONDITIONS: [&str; 6] = ["if", "when", "over", "under", "else", "otherwise"];
static SNAKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]*(_[a-z0-9]+)+$").unwrap());

struct Ctx<'a> {
    c: &'a Corpus,
    bounds: BTreeMap<&'static str, usize>,
    operations: BTreeSet<String>,
    boundary: Regex,
    out: Vec<Finding>,
}

impl Ctx<'_> {
    fn bound(&self, name: &str) -> usize {
        self.bounds[name]
    }

    fn push(&mut self, file: &str, line: usize, code: &str, msg: String) {
        self.out.push(Finding::new("diagram", file, line, code, msg));
    }
}

pub fn check(c: &Corpus) -> Vec<Finding> {
    let names: Vec<String> = c.reg.contracts.keys().map(|k| regex::escape(k)).collect();
    let mut bounds = BTreeMap::new();
    let mut missing = Vec::new();
    for name in BOUNDS {
        match c.reg.fragments.get("corpus").and_then(|fr| fr.limit.get(name)).and_then(|l| l.value.as_integer()) {
            Some(v) => {
                bounds.insert(name, v as usize);
            }
            None => missing.push(Finding::new("diagram", "spec/terms/corpus.toml", 0, "SpecRegistry", format!("bound `{name}` is missing; the diagram clauses read it"))),
        }
    }
    if !missing.is_empty() {
        return missing;
    }
    let mut x = Ctx {
        c,
        bounds,
        operations: c.reg.fragments.values().flat_map(|f| f.operation.keys().map(|o| o.replace('-', " "))).collect(),
        boundary: Regex::new(&format!(r"(?i)\b(({}) contract|boundary)\b", names.join("|"))).unwrap(),
        out: Vec::new(),
    };
    fence(&mut x);
    for fc in fences(c) {
        let rel = fc.doc.rel.as_str();
        match &fc.model {
            Model::Flowchart(f) => {
                for rule in FLOWCHART_RULES {
                    rule(&mut x, rel, f);
                }
            }
            Model::Sequence(s) => sequence_rules(&mut x, rel, s),
            Model::State(m) => state_rules(&mut x, rel, m),
            Model::Other => {}
        }
    }
    x.out
}

type FlowRule = fn(&mut Ctx, &str, &Flowchart);
const FLOWCHART_RULES: [FlowRule; 9] = [boundary, node, shape, edge, decision, branch, connected, unique_label, layout];

fn fence(x: &mut Ctx) {
    let boxes = |s: &str| s.chars().any(|ch| ('\u{2500}'..='\u{257F}').contains(&ch));
    for d in &x.c.docs {
        for (n, l, k) in d.each() {
            if !matches!(k, LineKind::Code | LineKind::Fence | LineKind::Front) && boxes(l) {
                x.out.push(Finding::new("diagram", &d.rel, n, "SpecAsciiDiagram", "box-drawing outside a mermaid fence".into()));
            }
        }
    }
}

fn boundary(x: &mut Ctx, rel: &str, f: &Flowchart) {
    for n in &f.nodes {
        if let Some(b) = x.boundary.find(&n.label).map(|m| m.as_str().to_string()) {
            x.push(rel, n.line, "SpecDiagramBoundary", format!("node `{}` stands for `{b}`; draw it as a `subgraph` holding its components", n.label));
        }
    }
}

fn node(x: &mut Ctx, rel: &str, f: &Flowchart) {
    let cap = x.bound("corpus-node-words");
    for n in f.nodes.iter().filter(|n| n.shape != Shape::Decision) {
        let text = plain(&n.label);
        let lower = text.to_lowercase().replace('-', " ");
        let second = lower.split_whitespace().nth(1).unwrap_or("");
        let bare = ENTITY.replace_all(&n.label, "");
        let why = if let Some(t) = ["·", ",", ";", ": "].iter().find(|t| bare.contains(**t)) {
            Some(format!("bundles attributes with `{}`", t.trim()))
        } else if BREAK.is_match(&n.label) {
            Some("stacks lines with `<br/>`".into())
        } else if let Some(e) = error_in(x.c, &n.label) {
            Some(format!("names error `{e}`; an error rides the edge that raises it"))
        } else if x.operations.contains(&lower) {
            Some("is an operation; an operation rides an edge".into())
        } else if ARTICLES.contains(&second) {
            Some("reads as a verb phrase; the verb rides an edge".into())
        } else if words(&n.label) > cap {
            Some(format!("holds {} words, over {cap}", words(&n.label)))
        } else {
            None
        };
        if let Some(why) = why {
            x.push(rel, n.line, "SpecDiagramNode", format!("node `{}` {why}; a node names one thing", n.label));
        }
    }
}

fn shape(x: &mut Ctx, rel: &str, f: &Flowchart) {
    for n in &f.nodes {
        let text = plain(&n.label);
        let last = text.split_whitespace().last().unwrap_or("").to_lowercase();
        if (SNAKE.is_match(&text) || STORAGE.contains(&last.as_str())) && n.shape != Shape::Cylinder {
            x.push(rel, n.line, "SpecDiagramShape", format!("node `{}` is a table or store; draw it as a cylinder `[( )]`", n.label));
        }
        if text.ends_with('?') && n.shape != Shape::Decision {
            x.push(rel, n.line, "SpecDiagramShape", format!("node `{}` asks a question; draw it as a decision `{{ }}`", n.label));
        }
    }
}

fn is_decision(f: &Flowchart, id: &str) -> bool {
    f.nodes.iter().any(|n| n.id == id && n.shape == Shape::Decision)
}

fn edge(x: &mut Ctx, rel: &str, f: &Flowchart) {
    let cap = x.bound("corpus-edge-words");
    for e in &f.edges {
        let name = format!("{} → {}", e.from, e.to);
        if e.two_way {
            x.push(rel, e.line, "SpecDiagramEdge", format!("edge `{name}` points both ways; draw one edge per direction"));
        }
        match &e.label {
            None if !is_decision(f, &e.from) && !is_decision(f, &e.to) => {
                x.push(rel, e.line, "SpecDiagramEdge", format!("edge `{name}` carries no label; name what flows or happens"));
            }
            Some(l) if l.contains('·') => x.push(rel, e.line, "SpecDiagramEdge", format!("edge `{name}` label `{l}` bundles attributes with `·`")),
            Some(l) if words(l) > cap => x.push(rel, e.line, "SpecDiagramEdge", format!("edge `{name}` label `{l}` holds {} words, over {cap}", words(l))),
            _ => {}
        }
    }
}

fn decision(x: &mut Ctx, rel: &str, f: &Flowchart) {
    let cap = x.bound("corpus-decision-words");
    for n in f.nodes.iter().filter(|n| n.shape == Shape::Decision) {
        let outs: Vec<&Edge> = f.edges.iter().filter(|e| e.from == n.id).collect();
        let mut why = Vec::new();
        if words(&n.label) > cap {
            why.push(format!("holds {} words, over {cap}", words(&n.label)));
        }
        if let Some(e) = error_in(x.c, &n.label) {
            why.push(format!("names error `{e}`"));
        }
        if !f.edges.iter().any(|e| e.to == n.id) {
            why.push("has no incoming edge".into());
        }
        if outs.len() < 2 {
            why.push(format!("has {} outgoing edge(s), under 2", outs.len()));
        }
        if outs.iter().any(|e| e.label.is_none()) {
            why.push("has an unlabelled exit".into());
        }
        let labels: Vec<String> = outs.iter().filter_map(|e| e.label.as_ref()).map(|l| plain(l).to_lowercase()).collect();
        if labels.iter().collect::<BTreeSet<_>>().len() < labels.len() {
            why.push("repeats an outcome label".into());
        }
        if !why.is_empty() {
            x.push(rel, n.line, "SpecDiagramDecision", format!("decision `{}` {}", n.label, why.join("; ")));
        }
    }
}

fn branch(x: &mut Ctx, rel: &str, f: &Flowchart) {
    for n in f.nodes.iter().filter(|n| n.shape != Shape::Decision) {
        let outs: Vec<&str> = f.edges.iter().filter(|e| e.from == n.id).filter_map(|e| e.label.as_deref()).collect();
        let exits = f.edges.iter().filter(|e| e.from == n.id).count();
        let firsts: Vec<String> = outs.iter().map(|l| first_word(l)).collect();
        let answers = firsts.iter().any(|w| w == "yes" || w == "no");
        let conditions = firsts.iter().filter(|w| CONDITIONS.contains(&w.as_str())).count();
        let errors = exits >= 2 && outs.iter().any(|l| error_in(x.c, l).is_some());
        if answers || conditions >= 2 || errors {
            x.push(rel, n.line, "SpecDiagramBranch", format!("node `{}` branches on a condition through its edge labels; draw the branch as a decision `{{ }}`", n.label));
        }
    }
}

fn connected(x: &mut Ctx, rel: &str, f: &Flowchart) {
    let ends: BTreeSet<&str> = f.edges.iter().flat_map(|e| [e.from.as_str(), e.to.as_str()]).collect();
    let parent: BTreeMap<&str, Option<&str>> = f.subgraphs.iter().map(|s| (s.id.as_str(), s.parent.as_deref())).collect();
    for n in &f.nodes {
        let mut at = Some(n.id.as_str());
        let mut up = n.parent.as_deref();
        let mut reached = false;
        while let Some(id) = at {
            if ends.contains(id) {
                reached = true;
                break;
            }
            at = up;
            up = up.and_then(|p| parent.get(p).copied().flatten());
        }
        if !reached {
            x.push(rel, n.line, "SpecDiagramOrphan", format!("node `{}` is no edge endpoint, nor is a subgraph holding it", n.label));
        }
    }
}

fn unique_label(x: &mut Ctx, rel: &str, f: &Flowchart) {
    let mut seen = BTreeSet::new();
    for n in &f.nodes {
        if !seen.insert(plain(&n.label).to_lowercase()) {
            x.push(rel, n.line, "SpecDiagramDuplicate", format!("node `{}` repeats another node's label; reuse that node's id", n.label));
        }
    }
}

fn layout(x: &mut Ctx, rel: &str, f: &Flowchart) {
    if !matches!(f.direction.as_deref(), Some("LR" | "TB" | "TD")) {
        x.push(rel, f.header_line, "SpecDiagramLayout", "flowchart header declares no `LR`, `TB` or `TD`".into());
    }
    for &l in &f.direction_lines {
        x.push(rel, l, "SpecDiagramLayout", "subgraph declares its own `direction`".into());
    }
    let depth = x.bound("corpus-subgraph-depth");
    for s in f.subgraphs.iter().filter(|s| s.depth > depth) {
        x.push(rel, s.line, "SpecDiagramLayout", format!("subgraph `{}` nests {} levels, over {depth}", s.id, s.depth));
    }
    let (nodes, edges) = (x.bound("corpus-chart-nodes"), x.bound("corpus-chart-edges"));
    if f.nodes.len() > nodes || f.edges.len() > edges {
        x.push(rel, f.header_line, "SpecDiagramLayout", format!("flowchart holds {} nodes and {} edges, over {nodes} and {edges}", f.nodes.len(), f.edges.len()));
    }
}

fn sequence_rules(x: &mut Ctx, rel: &str, s: &Sequence) {
    let declared: BTreeSet<&str> = s.declared.iter().map(|(p, _)| p.as_str()).collect();
    let used: BTreeSet<&str> = s.messages.iter().flat_map(|(a, b, _, _)| [a.as_str(), b.as_str()]).collect();
    let mut out = Vec::new();
    for p in used.difference(&declared) {
        let line = s.messages.iter().find(|m| m.0 == *p || m.1 == *p).map_or(s.header_line, |m| m.3);
        out.push((line, format!("participant `{p}` is messaged but never declared")));
    }
    for (p, line) in s.declared.iter().filter(|(p, _)| !used.contains(p.as_str())) {
        out.push((*line, format!("participant `{p}` is declared but never messaged")));
    }
    let (parts, msgs, selfs, depth) = (x.bound("corpus-participants"), x.bound("corpus-messages"), x.bound("corpus-self-messages"), x.bound("corpus-fragment-depth"));
    if declared.union(&used).count() > parts {
        out.push((s.header_line, format!("{} participants, over {parts}", declared.union(&used).count())));
    }
    if s.messages.len() > msgs {
        out.push((s.header_line, format!("{} messages, over {msgs}", s.messages.len())));
    }
    let own = s.messages.iter().filter(|m| m.0 == m.1).count();
    if own > selfs {
        out.push((s.header_line, format!("{own} messages from a participant to itself, over {selfs}")));
    }
    if s.max_fragment_depth.0 > depth {
        out.push((s.max_fragment_depth.1, format!("fragments nested {} levels, over {depth}", s.max_fragment_depth.0)));
    }
    for (line, msg) in out {
        x.push(rel, line, "SpecDiagramSequence", msg);
    }
    let (mw, nw) = (x.bound("corpus-message-words"), x.bound("corpus-note-words"));
    for (_, _, label, line) in s.messages.iter().filter(|m| words(&m.2) > mw) {
        x.push(rel, *line, "SpecDiagramMessage", format!("message `{label}` holds {} words, over {mw}", words(label)));
    }
    for (note, line) in s.notes.iter().filter(|n| words(&n.0) > nw) {
        x.push(rel, *line, "SpecDiagramMessage", format!("note `{note}` holds {} words, over {nw}", words(note)));
    }
}

fn state_rules(x: &mut Ctx, rel: &str, m: &StateMachine) {
    if m.starts.len() != 1 {
        x.push(rel, m.header_line, "SpecDiagramState", format!("state diagram has {} top-level `[*]` starts, not 1", m.starts.len()));
    }
    let mut reached: BTreeSet<&str> = BTreeSet::from(["[*]"]);
    let mut frontier = vec!["[*]"];
    while let Some(s) = frontier.pop() {
        for (_, to, _, _) in m.transitions.iter().filter(|t| t.0 == s) {
            if reached.insert(to.as_str()) {
                frontier.push(to.as_str());
            }
        }
    }
    for (s, line) in &m.states {
        if !reached.contains(s.as_str()) {
            x.push(rel, *line, "SpecDiagramState", format!("state `{s}` is unreachable from the start"));
        }
        if !m.leaving.contains(s) {
            x.push(rel, *line, "SpecDiagramState", format!("state `{s}` has no outgoing transition"));
        }
    }
    let cap = x.bound("corpus-transition-words");
    for (_, _, label, line) in m.transitions.iter().filter(|t| words(&t.2) > cap) {
        x.push(rel, *line, "SpecDiagramState", format!("transition `{label}` holds {} words, over {cap}", words(label)));
    }
}
