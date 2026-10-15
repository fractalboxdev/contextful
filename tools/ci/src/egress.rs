//! The egress lint (`assurance.gate.unconfigured-egress`): an outbound call — a path into
//! an HTTP or database client crate in any position, an imported or renamed client, a raw
//! component request, or a socket dial — sits only in a registered
//! adapter, each reached solely through the operator configuration its clause names.
//! Every other call site in a crate's sources is reachable without that configuration.

use anyhow::{Context, Result};
use proc_macro2::Span;
use std::fs;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Item, ItemExternCrate, ItemUse, Path as SynPath, UseTree};

use crate::refuse;
use crate::source_lint::{cfg_test, children};

/// Crates whose paths open outbound connections.
const CLIENTS: [&str; 11] =
    ["ureq", "reqwest", "hyper", "hyper_util", "isahc", "attohttpc", "curl", "surf", "awc", "postgres", "tokio_postgres"];
/// Path tails that dial a socket.
const DIALS: [[&str; 2]; 3] = [["TcpStream", "connect"], ["TcpStream", "connect_timeout"], ["UdpSocket", "bind"]];
/// Final path segments that send a request no adapter mediates.
const SENDS: [&str; 1] = ["default_send_request"];

/// The configured adapters: a source path prefix and the clause naming the configuration
/// that alone reaches it.
const CONFIGURED: [(&str, &str); 5] = [
    // Dialed only for a host a connector, component or model declaration names.
    ("crates/contextful-outbound/src/ureq_transport.rs", "connector.attach.transport-port"),
    // Opened only from a `[sync] endpoint` with an `s3://` or `r2://` scheme.
    ("crates/contextful-sync/src/s3_bucket.rs", "store.endpoint.schemes"),
    // Opened only under the `d1-catalog` backend an operator selects.
    ("crates/contextful-d1/src/lib.rs", "topology.coordinate.backends"),
    // Connected only under the `pg-catalog` backend an operator selects.
    ("crates/contextful-pg/src/", "topology.coordinate.backends"),
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
            let clients = clients(&package)?;
            check_tree(root, &src, &clients)?;
        }
    }
    Ok(())
}

/// The client crates `package` names: every listed crate, plus each dependency key its
/// manifest renames onto one through `package =`.
fn clients(package: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = CLIENTS.iter().map(|c| c.to_string()).collect();
    let manifest = package.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(out);
    }
    let text = fs::read_to_string(&manifest).with_context(|| format!("read {}", manifest.display()))?;
    let Ok(doc) = text.parse::<toml::Table>() else { return Ok(out) };
    let mut tables: Vec<&toml::Table> = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        tables.extend(doc.get(key).and_then(toml::Value::as_table));
    }
    if let Some(targets) = doc.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                tables.extend(target.get(key).and_then(toml::Value::as_table));
            }
        }
    }
    for table in tables {
        for (key, spec) in table {
            let Some(renamed) = spec.get("package").and_then(toml::Value::as_str) else { continue };
            if CLIENTS.contains(&renamed.replace('-', "_").as_str()) {
                out.push(key.replace('-', "_"));
            }
        }
    }
    Ok(out)
}

fn check_tree(root: &Path, dir: &Path, clients: &[String]) -> Result<()> {
    for path in children(dir)? {
        if path.is_dir() {
            check_tree(root, &path, clients)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if CONFIGURED.iter().any(|(prefix, _)| relative.starts_with(prefix)) {
                continue;
            }
            let source = fs::read_to_string(&path).with_context(|| format!("read {relative}"))?;
            let syntax = syn::parse_file(&source).with_context(|| format!("parse {relative}"))?;
            let mut visitor = Outbound { clients, first: None };
            visitor.visit_file(&syntax);
            if let Some((line, what)) = visitor.first {
                return Err(refuse("UnconfiguredEgress", format!("{relative}:{line} reaches `{what}` outside a configured adapter")));
            }
        }
    }
    Ok(())
}

struct Outbound<'a> {
    clients: &'a [String],
    first: Option<(usize, String)>,
}

impl Outbound<'_> {
    fn hit(&mut self, span: Span, what: String) {
        let line = span.start().line;
        if self.first.as_ref().is_none_or(|(at, _)| line < *at) {
            self.first = Some((line, what));
        }
    }

    fn client(&self, ident: &syn::Ident) -> bool {
        self.clients.iter().any(|c| ident == c)
    }

    /// The first client crate a use tree roots at, recursing into groups.
    fn use_root(&self, tree: &UseTree) -> Option<String> {
        match tree {
            UseTree::Path(p) => self.client(&p.ident).then(|| p.ident.to_string()),
            UseTree::Name(n) => self.client(&n.ident).then(|| n.ident.to_string()),
            UseTree::Rename(r) => self.client(&r.ident).then(|| r.ident.to_string()),
            UseTree::Group(g) => g.items.iter().find_map(|t| self.use_root(t)),
            UseTree::Glob(_) => None,
        }
    }

    fn outbound(&self, path: &SynPath) -> Option<String> {
        let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let text = segments.join("::");
        let first = path.segments.first()?;
        if self.client(&first.ident) && segments.len() > 1 {
            return Some(text);
        }
        if segments.last().is_some_and(|last| SENDS.contains(&last.as_str())) {
            return Some(text);
        }
        let tail: Vec<&str> = segments.iter().rev().take(2).rev().map(String::as_str).collect();
        DIALS.iter().any(|d| tail == d).then_some(text)
    }
}

impl<'ast> Visit<'ast> for Outbound<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let attrs: &[syn::Attribute] = match item {
            Item::Const(i) => &i.attrs,
            Item::Enum(i) => &i.attrs,
            Item::ExternCrate(i) => &i.attrs,
            Item::Fn(i) => &i.attrs,
            Item::ForeignMod(i) => &i.attrs,
            Item::Impl(i) => &i.attrs,
            Item::Macro(i) => &i.attrs,
            Item::Mod(i) => &i.attrs,
            Item::Static(i) => &i.attrs,
            Item::Struct(i) => &i.attrs,
            Item::Trait(i) => &i.attrs,
            Item::TraitAlias(i) => &i.attrs,
            Item::Type(i) => &i.attrs,
            Item::Union(i) => &i.attrs,
            Item::Use(i) => &i.attrs,
            _ => &[],
        };
        if !cfg_test(attrs) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if !cfg_test(&f.attrs) {
            syn::visit::visit_impl_item_fn(self, f);
        }
    }

    fn visit_item_use(&mut self, u: &'ast ItemUse) {
        if let Some(root) = self.use_root(&u.tree) {
            self.hit(u.span(), root);
        }
    }

    fn visit_item_extern_crate(&mut self, e: &'ast ItemExternCrate) {
        if self.client(&e.ident) {
            self.hit(e.span(), e.ident.to_string());
        }
    }

    /// Every path — expression, type, pattern or qualified self — rooted at a client.
    fn visit_path(&mut self, path: &'ast SynPath) {
        if let Some(what) = self.outbound(path) {
            self.hit(path.span(), what);
        }
        syn::visit::visit_path(self, path);
    }
}
