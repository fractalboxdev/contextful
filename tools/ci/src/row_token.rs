//! The row-token lint (`assurance.gate.row-token`): every public function of the store
//! crate returning the read response — the type every read surface carries rows to a
//! caller in — takes the enforcement admission value, and that value holds only private
//! fields, so its one constructor stays in the enforcement module.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{Attribute, FnArg, ImplItem, Item, ReturnType, Signature, Type, Visibility};

use crate::refuse;
use crate::source_lint::{children, cfg_test};

/// The store crate's sources.
const STORE: &str = "crates/contextful-context/src";
/// The types a row-returning function returns rows in.
const ROW_TYPES: [&str; 1] = ["Response"];
/// The enforcement admission value.
const TOKEN: &str = "Session";
/// The file defining the admission value, inside the enforcement module.
const TOKEN_HOME: &str = "crates/contextful-policy/src/enforce/session.rs";

/// Row-returning functions that reach no row of a registered relation, each with the
/// clause that fixes its path.
const UNMEDIATED: [(&str, &str); 3] = [
    // Operator text over the operator's own store registers every table raw.
    ("crates/contextful-context/src/read/face.rs::Face::operator_query", "read.query.project-relations"),
    // Operator text over no store: no stored row exists to mediate.
    ("crates/contextful-context/src/read/face.rs::operator_query", "read.guard.statement-provenance"),
    // The audit view projects audit entries, never a stored row.
    ("crates/contextful-context/src/read/audit.rs::audit_reads", "disclosure.record.reads-view"),
];

pub fn check(root: &Path) -> Result<()> {
    token_private(root)?;
    let store = root.join(STORE);
    if store.is_dir() {
        check_tree(root, &store)?;
    }
    Ok(())
}

fn token_private(root: &Path) -> Result<()> {
    let path = root.join(TOKEN_HOME);
    if !path.is_file() {
        return Ok(());
    }
    let source = fs::read_to_string(&path).with_context(|| format!("read {TOKEN_HOME}"))?;
    let syntax = syn::parse_file(&source).with_context(|| format!("parse {TOKEN_HOME}"))?;
    for item in &syntax.items {
        let Item::Struct(token) = item else { continue };
        if token.ident != TOKEN {
            continue;
        }
        let open = |why: String| refuse("EnforceUnmediatedPath", format!("{TOKEN_HOME}::{TOKEN} {why}"));
        if token.fields.is_empty() {
            return Err(open("holds no private field, so any crate constructs it".into()));
        }
        if let Some(field) = token.fields.iter().find(|f| !matches!(f.vis, Visibility::Inherited)) {
            let name = field.ident.as_ref().map_or_else(|| "a positional field".to_string(), |i| format!("field `{i}`"));
            return Err(open(format!("exposes {name} outside the enforcement module")));
        }
        if derives_default(&token.attrs) {
            return Err(open("derives `Default`, a constructor outside the enforcement module".into()));
        }
    }
    Ok(())
}

fn derives_default(attrs: &[Attribute]) -> bool {
    attrs.iter().filter(|a| a.path().is_ident("derive")).any(|a| {
        let mut found = false;
        let _ = a.parse_nested_meta(|meta| {
            found |= meta.path.is_ident("Default");
            Ok(())
        });
        found
    })
}

fn check_tree(root: &Path, dir: &Path) -> Result<()> {
    for path in children(dir)? {
        if path.is_dir() {
            check_tree(root, &path)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            let source = fs::read_to_string(&path).with_context(|| format!("read {relative}"))?;
            let syntax = syn::parse_file(&source).with_context(|| format!("parse {relative}"))?;
            let mut found = Vec::new();
            collect(&syntax.items, &mut found);
            for name in found {
                let site = format!("{relative}::{name}");
                if !UNMEDIATED.iter().any(|(allowed, _)| *allowed == site) {
                    return Err(refuse(
                        "EnforceUnmediatedPath",
                        format!("{site} returns rows and takes no `{TOKEN}`"),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Every public row-returning function in `items` taking no admission value, as
/// `Type::name` for a method.
fn collect(items: &[Item], found: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Fn(f) if !cfg_test(&f.attrs) && public(&f.vis) && bypasses(&f.sig) => {
                found.push(f.sig.ident.to_string());
            }
            Item::Impl(block) if block.trait_.is_none() && !cfg_test(&block.attrs) => {
                let owner = type_name(&block.self_ty);
                for member in &block.items {
                    if let ImplItem::Fn(f) = member {
                        if !cfg_test(&f.attrs) && public(&f.vis) && bypasses(&f.sig) {
                            found.push(format!("{owner}::{}", f.sig.ident));
                        }
                    }
                }
            }
            Item::Mod(m) if !cfg_test(&m.attrs) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, found);
                }
            }
            _ => {}
        }
    }
}

fn public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn bypasses(sig: &Signature) -> bool {
    let ReturnType::Type(_, out) = &sig.output else { return false };
    names(out, &ROW_TYPES)
        && !sig.inputs.iter().any(|arg| matches!(arg, FnArg::Typed(t) if names(&t.ty, &[TOKEN])))
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p.path.segments.last().map_or_else(String::new, |s| s.ident.to_string()),
        _ => String::from("_"),
    }
}

/// Whether `ty` names any of `idents` anywhere in its path segments or generic arguments.
fn names(ty: &Type, idents: &[&str]) -> bool {
    struct Names<'a> {
        idents: &'a [&'a str],
        hit: bool,
    }
    impl<'ast> Visit<'ast> for Names<'_> {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            self.hit |= self.idents.iter().any(|i| segment.ident == i);
            syn::visit::visit_path_segment(self, segment);
        }
    }
    let mut v = Names { idents, hit: false };
    v.visit_type(ty);
    v.hit
}
