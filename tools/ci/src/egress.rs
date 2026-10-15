//! The egress lint (`assurance.gate.unconfigured-egress`): an outbound call — a path into
//! an HTTP client crate, an imported client, or a socket dial — sits only in a registered
//! adapter, each reached solely through the operator configuration its clause names.
//! Every other call site in a crate's sources is reachable without that configuration.

use anyhow::{Context, Result};
use proc_macro2::Span;
use std::fs;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{ExprPath, ItemFn, ItemMod, ItemUse, Path as SynPath, UseTree};

use crate::refuse;
use crate::source_lint::{cfg_test, children};

/// Crates whose paths open outbound connections.
const CLIENTS: [&str; 9] = ["ureq", "reqwest", "hyper", "hyper_util", "isahc", "attohttpc", "curl", "surf", "awc"];
/// Path tails that dial a socket.
const DIALS: [[&str; 2]; 3] = [["TcpStream", "connect"], ["TcpStream", "connect_timeout"], ["UdpSocket", "bind"]];

/// The configured adapters: a source path prefix and the clause naming the configuration
/// that alone reaches it.
const CONFIGURED: [(&str, &str); 4] = [
    // Dialed only for a host a connector, component or model declaration names.
    ("crates/contextful-outbound/src/ureq_transport.rs", "connector.attach.transport-port"),
    // Opened only from a `[sync] endpoint` with an `s3://` or `r2://` scheme.
    ("crates/contextful-sync/src/s3_bucket.rs", "store.endpoint.schemes"),
    // Opened only under the `d1-catalog` backend an operator selects.
    ("crates/contextful-d1/src/lib.rs", "topology.coordinate.backends"),
    // The acceptance harness drives a built binary from tests and links into no binary.
    ("crates/acceptance/", "assurance.test.acceptance-surface"),
];

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
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if CONFIGURED.iter().any(|(prefix, _)| relative.starts_with(prefix)) {
                continue;
            }
            let source = fs::read_to_string(&path).with_context(|| format!("read {relative}"))?;
            let syntax = syn::parse_file(&source).with_context(|| format!("parse {relative}"))?;
            let mut visitor = Outbound::default();
            visitor.visit_file(&syntax);
            if let Some((line, what)) = visitor.first {
                return Err(refuse("UnconfiguredEgress", format!("{relative}:{line} reaches `{what}` outside a configured adapter")));
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Outbound {
    first: Option<(usize, String)>,
}

impl Outbound {
    fn hit(&mut self, span: Span, what: String) {
        let line = span.start().line;
        if self.first.as_ref().is_none_or(|(at, _)| line < *at) {
            self.first = Some((line, what));
        }
    }
}

impl<'ast> Visit<'ast> for Outbound {
    fn visit_item_mod(&mut self, m: &'ast ItemMod) {
        if !cfg_test(&m.attrs) {
            syn::visit::visit_item_mod(self, m);
        }
    }

    fn visit_item_fn(&mut self, f: &'ast ItemFn) {
        if !cfg_test(&f.attrs) {
            syn::visit::visit_item_fn(self, f);
        }
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if !cfg_test(&f.attrs) {
            syn::visit::visit_impl_item_fn(self, f);
        }
    }

    fn visit_item_use(&mut self, u: &'ast ItemUse) {
        let root = match &u.tree {
            UseTree::Path(p) => Some(&p.ident),
            UseTree::Name(n) => Some(&n.ident),
            UseTree::Rename(r) => Some(&r.ident),
            _ => None,
        };
        if let Some(root) = root.filter(|r| CLIENTS.iter().any(|c| *r == c)) {
            self.hit(u.span(), root.to_string());
        }
    }

    fn visit_expr_path(&mut self, e: &'ast ExprPath) {
        if let Some(what) = outbound(&e.path) {
            self.hit(e.span(), what);
        }
        syn::visit::visit_expr_path(self, e);
    }
}

fn outbound(path: &SynPath) -> Option<String> {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let text = segments.join("::");
    let first = segments.first()?;
    if CLIENTS.contains(&first.as_str()) && segments.len() > 1 {
        return Some(text);
    }
    let tail: Vec<&str> = segments.iter().rev().take(2).rev().map(String::as_str).collect();
    DIALS.iter().any(|d| tail == d).then_some(text)
}
