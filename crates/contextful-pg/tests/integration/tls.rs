//! A connection string's `sslrootcert` bundle replaces the WebPKI roots. A local rustls
//! listener answers PostgreSQL's TLS request with a certificate chained to the test CA in
//! `tests/fixtures/tls/`, so these run on every host without a database.

use contextful_pg::PgCatalog;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls").join(name)
}

/// A listener on a loopback port that answers one PostgreSQL TLS request with the
/// `localhost` certificate and reports whether the client completed the handshake.
fn tls_listener() -> (u16, mpsc::Receiver<Result<(), String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(fixture("server.pem")).unwrap().collect::<Result<_, _>>().unwrap();
    let key = PrivateKeyDer::from_pem_file(fixture("server.key.pem")).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else { return };
        socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut request = [0u8; 8];
        if socket.read_exact(&mut request).is_err() {
            let _ = tx.send(Err("no SSLRequest".into()));
            return;
        }
        socket.write_all(b"S").unwrap();
        let mut conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut outcome = Ok(());
        while conn.is_handshaking() {
            if let Err(e) = conn.complete_io(&mut socket) {
                outcome = Err(e.to_string());
                break;
            }
        }
        let _ = tx.send(outcome);
    });
    (port, rx)
}

fn handshake(rx: &mpsc::Receiver<Result<(), String>>) -> Result<(), String> {
    rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| Err("the client never reached the listener".into()))
}

fn ca() -> String {
    fixture("ca.pem").display().to_string()
}

// spec: topology.coordinate.postgres-ca-bundle@5b22398f
#[test]
fn an_sslrootcert_bundle_authenticates_a_server_the_webpki_roots_do_not() {
    let (port, rx) = tls_listener();
    let conninfo = format!("host=localhost hostaddr=127.0.0.1 port={port} user=worker dbname=catalog sslmode=require connect_timeout=5 sslrootcert='{}'", ca());
    // The listener closes after the handshake, so the open itself fails after TLS succeeds.
    let _ = PgCatalog::connect(&conninfo);
    handshake(&rx).unwrap_or_else(|e| panic!("the bundle's root did not authenticate the server: {e}"));

    let encoded: String = ca().bytes().map(|b| if b.is_ascii_alphanumeric() || b"/._-".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let (port, rx) = tls_listener();
    let url = format!("postgresql://worker@localhost:{port}/catalog?hostaddr=127.0.0.1&sslrootcert={encoded}&sslmode=require&connect_timeout=5");
    let _ = PgCatalog::connect(&url);
    handshake(&rx).unwrap_or_else(|e| panic!("the URL form's bundle did not authenticate the server: {e}"));

    let (port, rx) = tls_listener();
    let without = format!("host=localhost hostaddr=127.0.0.1 port={port} user=worker dbname=catalog sslmode=require connect_timeout=5");
    let failure = PgCatalog::connect(&without).err().expect("the WebPKI roots accepted the test CA");
    assert!(handshake(&rx).is_err(), "the WebPKI roots completed a handshake with the test CA");
    assert!(failure.message.contains("UnknownIssuer"), "{failure}");
}

#[test]
fn an_sslrootcert_bundle_still_verifies_the_hostname() {
    let (port, rx) = tls_listener();
    let conninfo = format!("host=db.example hostaddr=127.0.0.1 port={port} user=worker dbname=catalog connect_timeout=5 sslrootcert='{}'", ca());
    let failure = PgCatalog::connect(&conninfo).err().expect("a certificate for another name opened the catalog");
    assert!(handshake(&rx).is_err(), "a certificate for `localhost` authenticated `db.example`");
    // The client refuses the name, not the issuer: the bundle authenticated the chain.
    assert!(failure.message.contains("not valid for name \"db.example\""), "{failure}");
    assert!(!failure.message.contains("UnknownIssuer"), "{failure}");
}

// spec: topology.coordinate.postgres-ca-unreadable@85cca2d5
#[test]
fn an_unreadable_or_certificate_free_bundle_refuses_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.pem");
    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, "no certificate here\n").unwrap();
    let key = fixture("server.key.pem");
    // A closed port: a refusal that reached the network would report the connection instead.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    for path in [&missing, &empty, &key, &dir.path().to_path_buf()] {
        let path = path.display().to_string();
        for conninfo in [
            format!("host=db.example port={port} user=worker password=canary-secret dbname=catalog connect_timeout=5 sslrootcert='{path}'"),
            format!("postgres://worker:canary-secret@db.example:{port}/catalog?sslrootcert={path}&connect_timeout=5"),
        ] {
            for failure in [PgCatalog::validate_transport(&conninfo).unwrap_err(), PgCatalog::connect(&conninfo).err().expect("an unreadable bundle opened the catalog")] {
                assert!(failure.message.contains("DatabaseCaBundleUnreadable"), "{failure}");
                assert!(failure.message.contains(&path), "{failure}");
                assert!(!failure.message.contains("canary-secret"), "{failure}");
            }
        }
    }
}

// spec: topology.coordinate.postgres-verify-modes@82ae7d20
#[test]
fn verify_ca_and_verify_full_require_tls_and_verify_the_chain_and_hostname() {
    for mode in ["verify-ca", "verify-full"] {
        // Loopback: the mode still requires TLS, and the bundle's root authenticates the chain.
        let (port, rx) = tls_listener();
        let _ = PgCatalog::connect(&format!("host=localhost hostaddr=127.0.0.1 port={port} user=worker dbname=catalog connect_timeout=5 sslmode={mode} sslrootcert='{}'", ca()));
        handshake(&rx).unwrap_or_else(|e| panic!("sslmode={mode} did not complete TLS: {e}"));
        let (port, rx) = tls_listener();
        let _ = PgCatalog::connect(&format!("postgres://worker@localhost:{port}/catalog?hostaddr=127.0.0.1&sslmode={mode}&sslrootcert={}&connect_timeout=5", ca()));
        handshake(&rx).unwrap_or_else(|e| panic!("URL sslmode={mode} did not complete TLS: {e}"));

        let (port, _rx) = tls_listener();
        let failure = PgCatalog::connect(&format!("host=db.example hostaddr=127.0.0.1 port={port} user=worker dbname=catalog connect_timeout=5 sslmode={mode} sslrootcert='{}'", ca()))
            .err()
            .expect("a certificate for another name opened the catalog");
        assert!(failure.message.contains("not valid for name \"db.example\""), "sslmode={mode}: {failure}");

        let (port, _rx) = tls_listener();
        let failure = PgCatalog::connect(&format!("host=localhost hostaddr=127.0.0.1 port={port} user=worker dbname=catalog connect_timeout=5 sslmode={mode}"))
            .err()
            .expect("the WebPKI roots accepted the test CA");
        assert!(failure.message.contains("UnknownIssuer"), "sslmode={mode}: {failure}");
    }
}

// spec: topology.coordinate.postgres-system-roots@122660d8
#[test]
fn sslrootcert_system_selects_the_webpki_roots_and_reads_no_file() {
    for url in [false, true] {
        let (port, rx) = tls_listener();
        let conninfo = if url {
            format!("postgres://worker@localhost:{port}/catalog?hostaddr=127.0.0.1&sslrootcert=system&sslmode=require&connect_timeout=5")
        } else {
            format!("host=localhost hostaddr=127.0.0.1 port={port} user=worker dbname=catalog sslmode=require connect_timeout=5 sslrootcert=system")
        };
        PgCatalog::validate_transport(&conninfo).unwrap_or_else(|e| panic!("sslrootcert=system was read as a path: {e}"));
        let failure = PgCatalog::connect(&conninfo).err().expect("the WebPKI roots accepted the test CA");
        assert!(handshake(&rx).is_err(), "the WebPKI roots completed a handshake with the test CA");
        assert!(failure.message.contains("UnknownIssuer"), "{failure}");
    }
}
