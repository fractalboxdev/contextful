//! The row-token lint (`assurance.gate.row-token`): every public function of the store
//! crate returning or handing out stored rows — the read response, a record batch, or a
//! vector of row maps, as its result or a callback's argument — takes the enforcement
//! admission value by reference, and that value holds only private fields with no public
//! constructor but the one taking an admitted authority.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use syn::visit::Visit;
use syn::{Attribute, FnArg, ImplItem, Item, PathArguments, ReturnType, Signature, Type, Visibility};

use crate::refuse;
use crate::source_lint::{children, cfg_test};

/// The store crate's sources.
const STORE: &str = "crates/contextful-context/src";
/// The store crate's manifest, whose dependency on the policy crate names the token's home.
const STORE_MANIFEST: &str = "crates/contextful-context/Cargo.toml";
/// The crate holding the enforcement module.
const POLICY: &str = "contextful-policy";
/// The types a row-returning function returns rows in, besides a vector of row maps.
const ROW_TYPES: [&str; 2] = ["Response", "RecordBatch"];
/// The enforcement admission value.
const TOKEN: &str = "Session";
/// The file defining the admission value, inside the enforcement module.
const TOKEN_HOME: &str = "crates/contextful-policy/src/enforce/session.rs";
/// The input every public constructor of the admission value takes.
const AUTHORITY: &str = "AdmittedAuthority";

/// Row-returning functions that reach no row of a registered relation, each with the
/// clause that fixes its path.
const UNMEDIATED: &[(&str, &str)] = &[
    // Operator text over the operator's own store registers every table raw.
    ("crates/contextful-context/src/read/face.rs::Face::operator_query", "read.query.project-relations"),
    // Operator text over no store: no stored row exists to mediate.
    ("crates/contextful-context/src/read/face.rs::operator_query", "read.guard.statement-provenance"),
    // The audit view projects audit entries, never a stored row.
    ("crates/contextful-context/src/read/audit.rs::audit_reads", "disclosure.record.reads-view"),
    // The run path selects a parent table's rows under the run's capability allowlist and
    // journal; an admitted run reads through the read face instead.
    ("crates/contextful-context/src/rows.rs::table_rows", "run.select.parent-scan"),
    ("crates/contextful-context/src/rows.rs::table_row_batches", "run.select.parent-scan"),
    // Converts a batch its caller already holds; reads no store.
    ("crates/contextful-context/src/rows.rs::batch_rows", "run.select.parent-scan"),
    // The Parquet codec under the store: the read engine and the run path reach part files
    // only through it, each behind its own enforcement crossing.
    ("crates/contextful-context/src/parquet_io.rs::read", "store.lay-out.components"),
    ("crates/contextful-context/src/parquet_io.rs::read_encrypted", "store.encrypt.cipher"),
    ("crates/contextful-context/src/parquet_io.rs::read_with_key", "store.encrypt.cipher"),
    ("crates/contextful-context/src/parquet_io.rs::each_batch_with_key", "store.encrypt.cipher"),
    ("crates/contextful-context/src/parquet_io.rs::read_bytes", "store.lay-out.components"),
    ("crates/contextful-context/src/parquet_io.rs::conform", "store.lay-out.schema-file"),
];

pub fn check(root: &Path) -> Result<()> {
    let store = root.join(STORE);
    if store.is_dir() {
        check_tree(root, &store)?;
    }
    token_private(root, store.is_dir() && takes_token(root)?)
}

/// Whether the store crate depends on the policy crate, so its row paths name the token.
fn takes_token(root: &Path) -> Result<bool> {
    let path = root.join(STORE_MANIFEST);
    if !path.is_file() {
        return Ok(false);
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {STORE_MANIFEST}"))?;
    let doc: toml::Table = text.parse().with_context(|| format!("parse {STORE_MANIFEST}"))?;
    Ok(doc.get("dependencies").and_then(toml::Value::as_table).is_some_and(|d| d.contains_key(POLICY)))
}

fn token_private(root: &Path, store: bool) -> Result<()> {
    let path = root.join(TOKEN_HOME);
    let open = |why: String| refuse("EnforceUnmediatedPath", format!("{TOKEN_HOME}::{TOKEN} {why}"));
    if !path.is_file() {
        return if store { Err(open("is absent, so no row path names an enforcement token".into())) } else { Ok(()) };
    }
    let source = fs::read_to_string(&path).with_context(|| format!("read {TOKEN_HOME}"))?;
    let syntax = syn::parse_file(&source).with_context(|| format!("parse {TOKEN_HOME}"))?;
    let mut defined = false;
    for item in &syntax.items {
        match item {
            Item::Struct(token) if token.ident == TOKEN => {
                defined = true;
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
            Item::Impl(block) if type_name(&block.self_ty) == TOKEN && !cfg_test(&block.attrs) => {
                if let Some((_, path, _)) = &block.trait_ {
                    let name = path.segments.last().map_or_else(String::new, |s| s.ident.to_string());
                    if name == "Default" || name == "From" {
                        return Err(open(format!("implements `{name}`, a constructor outside the enforcement module")));
                    }
                    continue;
                }
                for member in &block.items {
                    if let ImplItem::Fn(f) = member {
                        if public(&f.vis) && constructs(&f.sig) {
                            return Err(open(format!("has the public constructor `{}` taking no `{AUTHORITY}`", f.sig.ident)));
                        }
                    }
                }
            }
            Item::Fn(f) if public(&f.vis) && constructs(&f.sig) => {
                return Err(open(format!("has the public constructor `{}` taking no `{AUTHORITY}`", f.sig.ident)));
            }
            _ => {}
        }
    }
    if store && !defined {
        return Err(open("is not defined, so no row path names an enforcement token".into()));
    }
    Ok(())
}

/// Whether `sig` returns the admission value while taking no admitted authority.
fn constructs(sig: &Signature) -> bool {
    let ReturnType::Type(_, out) = &sig.output else { return false };
    names(out, &[TOKEN, "Self"]) && !sig.inputs.iter().any(|arg| matches!(arg, FnArg::Typed(t) if names(&t.ty, &[AUTHORITY])))
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
    let returns = matches!(&sig.output, ReturnType::Type(_, out) if row_shaped(out));
    let hands = sig.inputs.iter().any(|arg| matches!(arg, FnArg::Typed(t) if hands_rows(&t.ty)));
    (returns || hands) && !sig.inputs.iter().any(|arg| matches!(arg, FnArg::Typed(t) if is_token(&t.ty)))
}

/// Whether `ty`, behind any references, is the bare admission value: a path whose last
/// segment is the token with no generic wrapper around it.
fn is_token(ty: &Type) -> bool {
    match ty {
        Type::Reference(r) => is_token(&r.elem),
        Type::Paren(p) => is_token(&p.elem),
        Type::Path(p) => p.qself.is_none() && p.path.segments.last().is_some_and(|s| s.ident == TOKEN && s.arguments.is_empty()),
        _ => false,
    }
}

/// Whether `ty` carries stored rows: the read response, a record batch, or a vector of
/// row maps, anywhere in it.
fn row_shaped(ty: &Type) -> bool {
    struct Rows(bool);
    impl<'ast> Visit<'ast> for Rows {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            self.0 |= ROW_TYPES.iter().any(|i| segment.ident == i);
            if segment.ident == "Vec" {
                if let PathArguments::AngleBracketed(args) = &segment.arguments {
                    self.0 |= args.args.iter().any(|a| matches!(a, syn::GenericArgument::Type(t) if last_is(t, "Map")));
                }
            }
            syn::visit::visit_path_segment(self, segment);
        }
    }
    let mut v = Rows(false);
    v.visit_type(ty);
    v.0
}

fn last_is(ty: &Type, ident: &str) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == ident))
}

/// Whether `ty` holds a callback — `Fn*(..)` sugar or a bare `fn(..)` — whose arguments
/// carry stored rows.
fn hands_rows(ty: &Type) -> bool {
    struct Hands(bool);
    impl<'ast> Visit<'ast> for Hands {
        fn visit_parenthesized_generic_arguments(&mut self, args: &'ast syn::ParenthesizedGenericArguments) {
            self.0 |= args.inputs.iter().any(row_shaped);
            syn::visit::visit_parenthesized_generic_arguments(self, args);
        }
        fn visit_type_bare_fn(&mut self, f: &'ast syn::TypeBareFn) {
            self.0 |= f.inputs.iter().any(|a| row_shaped(&a.ty));
            syn::visit::visit_type_bare_fn(self, f);
        }
    }
    let mut v = Hands(false);
    v.visit_type(ty);
    v.0
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
