//! The TLS roots a connection authenticates its server against: the WebPKI roots, or the
//! PEM bundle the connection string's `sslrootcert` names in their place
//! (`topology.coordinate.postgres-ca-bundle`). The driver's parser knows no `sslrootcert`,
//! so the parameter leaves the string before the driver reads it.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use std::borrow::Cow;
use std::sync::Arc;

const PARAMETER: &str = "sslrootcert";

/// `conninfo` without its `sslrootcert` parameters, and the last one's path. A key-value
/// string the scan cannot tokenize passes through whole, so the driver reports the error.
pub(crate) fn split_root_cert(conninfo: &str) -> (Cow<'_, str>, Option<String>) {
    match ["postgres://", "postgresql://"].iter().find(|p| conninfo.starts_with(*p)) {
        Some(_) => split_url(conninfo),
        None => split_key_value(conninfo).map_or((Cow::Borrowed(conninfo), None), |(rest, path)| (Cow::Owned(rest), path)),
    }
}

fn split_url(url: &str) -> (Cow<'_, str>, Option<String>) {
    // The driver takes credentials up to the first `@`, so the query starts after it.
    let after = url.find('@').map_or(0, |at| at + 1);
    let Some(q) = url[after..].find('?').map(|q| after + q) else { return (Cow::Borrowed(url), None) };
    let (mut kept, mut path) = (Vec::new(), None);
    for pair in url[q + 1..].split('&') {
        match pair.split_once('=') {
            Some((PARAMETER, value)) => path = Some(percent_decode(value)),
            _ => kept.push(pair),
        }
    }
    if path.is_none() {
        return (Cow::Borrowed(url), None);
    }
    let base = &url[..q];
    (Cow::Owned(if kept.is_empty() { base.to_string() } else { format!("{base}?{}", kept.join("&")) }), path)
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
fn split_key_value(s: &str) -> Option<(String, Option<String>)> {
    let mut it = s.char_indices().peekable();
    let (mut rest, mut path, mut kept_from) = (String::new(), None, 0);
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
        if key == PARAMETER {
            let end = it.peek().map_or(s.len(), |&(i, _)| i);
            rest.push_str(&s[kept_from..start]);
            kept_from = end;
            path = Some(value);
        }
    }
    rest.push_str(&s[kept_from..]);
    Some((rest, path))
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
