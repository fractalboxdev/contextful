//! `contextful-spec scaffold`: one failing test per refusal and limit clause of an
//! operation, each tagged with its clause id and statement revision.

use crate::corpus::*;
use crate::util::statement_rev;
use anyhow::{bail, Result};
use regex::Regex;
use std::fmt::Write as _;
use std::path::Path;

/// Rust keywords a subject slug can collide with; those names take the `r#` form.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn", "else",
    "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match",
    "mod", "move", "mut", "override", "priv", "pub", "ref", "return", "static", "struct", "trait", "true", "try",
    "type", "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

fn ident(slug: &str) -> String {
    let name = slug.replace('-', "_");
    if KEYWORDS.contains(&name.as_str()) {
        format!("r#{name}")
    } else {
        name
    }
}

/// The placeholder a clause's test body holds until the test is written.
fn placeholder(c: &Corpus, cl: &Clause) -> Option<String> {
    let frag = c.reg.fragments.get(&cl.contract)?;
    let errors: Vec<&str> = frag.error.iter().filter(|(_, e)| e.clause == cl.id).map(|(k, _)| k.as_str()).collect();
    if !errors.is_empty() {
        return Some(format!("{}: assert {}", cl.id, errors.join(", ")));
    }
    let bounds: Vec<String> =
        frag.limit.values().filter(|l| l.clause == cl.id).map(|l| format!("{} {}", l.value_text(), l.unit)).collect();
    if !bounds.is_empty() {
        return Some(format!("{}: hold {}", cl.id, bounds.join(", ")));
    }
    None
}

fn function(cl: &Clause, todo: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "/// {}", cl.statement);
    let _ = writeln!(s, "// spec: {}@{}", cl.id, statement_rev(&cl.statement));
    s.push_str("#[test]\n");
    let _ = writeln!(s, "fn {}() {{", ident(&cl.subject));
    let _ = writeln!(s, "    todo!({:?});", todo);
    s.push_str("}\n");
    s
}

/// Insert `mod <name>;` into a `main.rs`, in order among its other `mod` lines.
fn with_mod_line(main: &str, name: &str) -> String {
    let line = format!("mod {name};");
    let lines: Vec<&str> = main.lines().collect();
    if lines.iter().any(|l| l.trim() == line) {
        return main.to_string();
    }
    let is_mod = |l: &str| l.starts_with("mod ") && l.ends_with(';');
    let at = match lines.iter().position(|l| is_mod(l) && *l > line.as_str()) {
        Some(i) => i,
        None => match lines.iter().rposition(|l| is_mod(l)) {
            Some(i) => i + 1,
            None => lines.iter().take_while(|l| l.starts_with("//!") || l.trim().is_empty()).count(),
        },
    };
    let mut out: Vec<&str> = lines[..at].to_vec();
    out.push(&line);
    out.extend_from_slice(&lines[at..]);
    let mut text = out.join("\n");
    text.push('\n');
    text
}

pub fn run(c: &Corpus, target: &str, package: &Path) -> Result<()> {
    let Some((contract, operation)) = target.split_once('.') else {
        bail!("`{target}` is not `<contract>.<operation>`")
    };
    let known = c.reg.fragments.get(contract).map(|f| f.operation.contains_key(operation)).unwrap_or(false);
    if !known {
        bail!("`{target}` names no registered operation");
    }
    let clauses: Vec<(&Clause, String)> = c
        .clauses()
        .filter(|cl| cl.contract == contract && cl.operation == operation)
        .filter_map(|cl| placeholder(c, cl).map(|t| (cl, t)))
        .collect();
    if clauses.is_empty() {
        eprintln!("`{target}` holds no refusal or limit clause");
        return Ok(());
    }

    let module = operation.replace('-', "_");
    let dir = package.join("tests/integration");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{module}.rs"));
    let mut text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| format!("//! `{target}`: one test per refusal and limit clause.\n"));
    let (mut wrote, mut kept) = (0, 0);
    for (cl, todo) in &clauses {
        let name = ident(&cl.subject);
        let defined = Regex::new(&format!(r"\bfn\s+{}\s*\(", regex::escape(&name))).unwrap();
        if defined.is_match(&text) {
            kept += 1;
            continue;
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
        text.push_str(&function(cl, todo));
        wrote += 1;
    }
    std::fs::write(&path, &text)?;

    let main = dir.join("main.rs");
    let before = std::fs::read_to_string(&main).unwrap_or_default();
    let after = with_mod_line(&before, &module);
    if after != before {
        std::fs::write(&main, after)?;
    }
    eprintln!("{}: wrote {wrote} test(s), kept {kept} existing", path.display());
    Ok(())
}
