//! Source-level refusals over runtime crates (`assurance.gate.interpolated-claim`).

use anyhow::{Context, Result};
use proc_macro2::Span;
use std::fs;
use std::path::{Path, PathBuf};
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Expr, Lit, Macro, Token};

use crate::refuse;

pub fn check(root: &Path) -> Result<()> {
    let crates = root.join("crates");
    if !crates.exists() {
        return Ok(());
    }
    for package in children(&crates)? {
        let src = package.join("src");
        if src.is_dir() {
            check_tree(root, &src)?;
        }
    }
    Ok(())
}

fn check_tree(root: &Path, dir: &Path) -> Result<()> {
    for path in children(dir)? {
        if path.is_dir() {
            check_tree(root, &path)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let source = fs::read_to_string(&path)
                .with_context(|| format!("read {}", relative.display()))?;
            let syntax = syn::parse_file(&source)
                .with_context(|| format!("parse {}", relative.display()))?;
            let mut visitor = SubjectSql::default();
            visitor.visit_file(&syntax);
            if let Some(line) = visitor.first_line {
                return Err(refuse(
                    "EnforceInterpolatedSubjectClaim",
                    format!("{}:{line}", relative.display()),
                ));
            }
        }
    }
    Ok(())
}

fn children(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    out.sort();
    Ok(out)
}

#[derive(Default)]
struct SubjectSql {
    first_line: Option<usize>,
}

impl<'ast> Visit<'ast> for SubjectSql {
    fn visit_macro(&mut self, macro_: &'ast Macro) {
        if self.first_line.is_some()
            || !(macro_.path.is_ident("format") || macro_.path.is_ident("format_args"))
        {
            return;
        }
        let parser = syn::punctuated::Punctuated::<Expr, Token![,]>::parse_terminated;
        let Ok(args) = parser.parse2(macro_.tokens.clone()) else {
            return;
        };
        let Some(Expr::Lit(first)) = args.first() else {
            return;
        };
        let Lit::Str(template) = &first.lit else {
            return;
        };
        let text = template.value();
        if sql_text(&text) && (captured_subject(&text) || args.iter().skip(1).any(uses_subject)) {
            self.first_line = Some(line(macro_.span()));
        }
    }
}

fn line(span: Span) -> usize {
    span.start().line
}

fn sql_text(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    [
        "SELECT ", "INSERT ", "UPDATE ", "DELETE ", "WITH ", "CREATE ", "ALTER ", "DROP ",
    ]
    .iter()
    .any(|keyword| upper.contains(keyword))
}

fn captured_subject(template: &str) -> bool {
    let bytes = template.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'{' {
            at += 1;
        } else if bytes.get(at + 1) == Some(&b'{') {
            at += 2;
        } else {
            let capture = &template[at + 1..];
            if capture.starts_with("subject}")
                || capture.starts_with("subject:")
                || capture.starts_with("subject.")
                || capture.starts_with("subject_")
            {
                return true;
            }
            at += 1;
        }
    }
    false
}

fn uses_subject(expr: &Expr) -> bool {
    #[derive(Default)]
    struct Subject(bool);
    impl<'ast> Visit<'ast> for Subject {
        fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
            self.0 |= path
                .path
                .segments
                .iter()
                .any(|segment| segment.ident.to_string().starts_with("subject"));
            syn::visit::visit_expr_path(self, path);
        }
        fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
            if let syn::Member::Named(name) = &field.member {
                self.0 |= name.to_string().starts_with("subject");
            }
            syn::visit::visit_expr_field(self, field);
        }
    }
    let mut subject = Subject::default();
    subject.visit_expr(expr);
    subject.0
}
