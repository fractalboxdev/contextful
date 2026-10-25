//! The TLS roots a connection authenticates its server against: the WebPKI roots, or the
//! PEM bundle the connection string's `sslrootcert` names in their place
//! (`topology.coordinate.postgres-ca-bundle`). The driver's parser knows no `sslrootcert`
//! and no `verify-ca` or `verify-full` mode, so the string is rewritten before the driver
//! reads it: `sslrootcert` leaves it, and a verifying mode becomes `require`, whose TLS
//! already verifies the chain and the hostname (`topology.coordinate.postgres-verify-modes`).

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use std::sync::Arc;

/// What happens to one parameter of the connection string.
enum Rewrite {
    Keep,
    Drop,
    Value(&'static str),
}

/// The connection string the driver parses, and the `sslrootcert` path it carried, absent
/// for libpq's `system` value (`topology.coordinate.postgres-system-roots`). A key-value
/// string the scan cannot tokenize passes through whole, so the driver reports the error.
pub(crate) fn driver_conninfo(conninfo: &str) -> (String, Option<String>) {
    let mut root_cert = None;
    let mut rewrite = |key: &str, value: String| match (key, value.as_str()) {
        ("sslrootcert", _) => {
            root_cert = Some(value);
            Rewrite::Drop
        }
        ("sslmode", "verify-ca" | "verify-full") => Rewrite::Value("require"),
        _ => Rewrite::Keep,
    };
    let rewritten = if ["postgres://", "postgresql://"].iter().any(|p| conninfo.starts_with(p)) {
        rewrite_url(conninfo, &mut rewrite)
    } else {
        rewrite_key_value(conninfo, &mut rewrite).unwrap_or_else(|| conninfo.to_string())
    };
    (rewritten, root_cert.filter(|path| path != "system"))
}

fn rewrite_url(url: &str, rewrite: &mut dyn FnMut(&str, String) -> Rewrite) -> String {
    // The driver takes credentials up to the first `@`, so the query starts after it.
    let after = url.find('@').map_or(0, |at| at + 1);
    let Some(q) = url[after..].find('?').map(|q| after + q) else { return url.to_string() };
    let mut kept = Vec::new();
    for pair in url[q + 1..].split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            kept.push(pair.to_string());
            continue;
        };
        match rewrite(&percent_decode(key), percent_decode(value)) {
            Rewrite::Keep => kept.push(pair.to_string()),
            Rewrite::Drop => {}
            Rewrite::Value(value) => kept.push(format!("{key}={value}")),
        }
    }
    let base = &url[..q];
    if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The key-value scan the driver performs: `key = value`, where a value is single-quoted
/// or runs to whitespace, and a backslash escapes the next character.
fn rewrite_key_value(s: &str, rewrite: &mut dyn FnMut(&str, String) -> Rewrite) -> Option<String> {
    let mut it = s.char_indices().peekable();
    let (mut rest, mut kept_from) = (String::new(), 0);
    loop {
        while it.next_if(|&(_, c)| c.is_whitespace()).is_some() {}
        let Some(&(start, _)) = it.peek() else { break };
        let mut key_end = start;
        while let Some((i, c)) = it.next_if(|&(_, c)| !c.is_whitespace() && c != '=') {
            key_end = i + c.len_utf8();
        }
        let key = &s[start..key_end];
        while it.next_if(|&(_, c)| c.is_whitespace()).is_some() {}
        it.next_if(|&(_, c)| c == '=')?;
        while it.next_if(|&(_, c)| c.is_whitespace()).is_some() {}
        let quoted = it.next_if(|&(_, c)| c == '\'').is_some();
        let mut value = String::new();
        loop {
            match it.peek().copied() {
                Some((_, '\'')) if quoted => {
                    it.next();
                    break;
                }
                Some((_, c)) if !quoted && c.is_whitespace() => break,
                None if quoted => return None,
                None => break,
                Some((_, '\\')) => {
                    it.next();
                    if let Some((_, c)) = it.next() {
                        value.push(c);
                    }
                }
                Some((_, c)) => {
                    it.next();
                    value.push(c);
                }
            }
        }
        let replacement = match rewrite(key, value) {
            Rewrite::Keep => continue,
            Rewrite::Drop => String::new(),
            Rewrite::Value(value) => format!("{key}={value}"),
        };
        let end = it.peek().map_or(s.len(), |&(i, _)| i);
        rest.push_str(&s[kept_from..start]);
        rest.push_str(&replacement);
        kept_from = end;
    }
    rest.push_str(&s[kept_from..]);
    Some(rest)
}

/// The client configuration for a connection: WebPKI roots, or the bundle at `root_cert`
/// alone. Hostname verification applies to either.
pub(crate) fn client_config(root_cert: Option<&str>) -> Result<ClientConfig, String> {
    let roots = match root_cert {
        None => RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        Some(path) => bundle(path)?,
    };
    let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the configured TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

/// The roots of the PEM bundle at `path`, refused when it is unreadable or holds no
/// certificate (`topology.coordinate.postgres-ca-unreadable`).
fn bundle(path: &str) -> Result<RootCertStore, String> {
    let unreadable = |why: &dyn std::fmt::Display| format!("DatabaseCaBundleUnreadable: sslrootcert `{path}` {why}");
    let pem = std::fs::read(path).map_err(|e| unreadable(&format_args!("is unreadable: {e}")))?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&pem).filter_map(Result::ok).collect();
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(certs);
    if added == 0 {
        return Err(unreadable(&"holds no certificate"));
    }
    Ok(roots)
}
