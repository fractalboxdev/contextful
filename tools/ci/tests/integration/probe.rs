//! `contextful-ci deploy probe`: `topology.publish-hostname`'s descriptors, probe table and
//! posture probe, driven against loopback servers standing in for published hostnames.

use crate::{stderr, Repo};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// A loopback host answering every request with `status`, recording each request head.
struct Host {
    url: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Host {
    fn answering(status: u16) -> Host {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let heads: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = heads.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                seen.lock().unwrap().push(head);
                let location = if status == 302 { "Location: https://login.example.org/\r\n" } else { "" };
                let _ = write!(stream, "HTTP/1.1 {status} X\r\n{location}Content-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        Host { url, heads }
    }

    fn requests(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

/// A URL no listener holds: the port is bound, read, then released.
fn unreachable() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", l.local_addr().unwrap())
}

fn descriptor(hostname: &str, gate: &str) -> String {
    format!("version = 1\nhostname = \"{hostname}\"\nworker = \"gateway\"\ngate = \"{gate}\"\nacknowledged = true\n")
}

/// Write one descriptor per `(hostname, gate)` and a probe table naming the same pairs.
fn declare(r: &Repo, pairs: &[(&str, &str)]) {
    let mut table = String::new();
    for (host, gate) in pairs {
        r.write(&format!("deploy/hostnames/{host}.toml"), &descriptor(host, gate));
        table.push_str(&format!("[[probe]]\nhostname = \"{host}\"\ngate = \"{gate}\"\n\n"));
    }
    r.write("deploy/probe.toml", &table);
}

fn probe(r: &Repo, resolve: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_contextful-ci"));
    cmd.args(["deploy", "probe"]).current_dir(&r.root);
    for (host, url) in resolve {
        cmd.args(["--resolve", &format!("{host}={url}")]);
    }
    cmd.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// A probe answer outside its descriptor's gate, or an unreachable hostname, raises `HostnamePostureMismatch` with the hostname, the declared gate and the observed response, and fails the deploy.
// spec: topology.publish-hostname.posture-mismatch@f7a60d89
#[test]
fn an_answer_outside_the_declared_gate_or_an_unreachable_hostname_fails_the_probe() {
    let r = Repo::init();
    let login = Host::answering(302);
    let broken = Host::answering(502);
    let gone = unreachable();
    declare(&r, &[("store.example.org", "public"), ("admin.example.org", "adminToken"), ("gone.example.org", "access")]);
    let o = probe(&r, &[("store.example.org", &login.url), ("admin.example.org", &broken.url), ("gone.example.org", &gone)]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("HostnamePostureMismatch"), "{err}");
    assert!(err.contains("store.example.org") && err.contains("`public`") && err.contains("302"), "{err}");
    assert!(err.contains("admin.example.org") && err.contains("`adminToken`") && err.contains("502"), "{err}");
    assert!(err.contains("gone.example.org") && err.contains("`access`") && err.contains("unreachable"), "{err}");
}

/// Each gate passes on its own answer, the probe follows no redirect, and it sends no
/// credential.
#[test]
fn each_gate_passes_on_its_answer_to_an_anonymous_get() {
    let r = Repo::init();
    let login = Host::answering(302);
    let token = Host::answering(401);
    let missing = Host::answering(404);
    let open = Host::answering(200);
    declare(
        &r,
        &[("sso.example.org", "access"), ("admin.example.org", "adminToken"), ("ops.example.org", "adminToken"), ("store.example.org", "public")],
    );
    let o = probe(
        &r,
        &[("sso.example.org", &login.url), ("admin.example.org", &token.url), ("ops.example.org", &missing.url), ("store.example.org", &open.url)],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    for (host, status) in [("sso.example.org", "302"), ("admin.example.org", "401"), ("ops.example.org", "404"), ("store.example.org", "200")] {
        assert!(out.lines().any(|l| l.contains(host) && l.contains(status)), "{host}: {out}");
    }
    let heads = login.requests();
    assert_eq!(heads.len(), 1, "a redirect is followed or retried: {heads:?}");
    let head = heads[0].to_ascii_lowercase();
    assert!(head.starts_with("get / "), "{head}");
    assert!(!head.contains("authorization:") && !head.contains("cookie:"), "{head}");
}

/// A probe table entry absent from the descriptor set, or a descriptor with no probe entry, raises `ProbeTableDrift` before the deploy runs, naming the hostname and the side missing it.
// spec: topology.publish-hostname.probe-table@244a9a2e
#[test]
fn a_probe_table_and_descriptor_set_that_differ_are_refused_before_any_probe() {
    let r = Repo::init();
    let open = Host::answering(200);
    r.write("deploy/hostnames/store.toml", &descriptor("store.example.org", "public"));
    r.write("deploy/hostnames/admin.toml", &descriptor("admin.example.org", "adminToken"));
    r.write(
        "deploy/probe.toml",
        "[[probe]]\nhostname = \"store.example.org\"\ngate = \"access\"\n\n[[probe]]\nhostname = \"docs.example.org\"\ngate = \"public\"\n",
    );
    let o = probe(&r, &[("store.example.org", &open.url), ("admin.example.org", &open.url), ("docs.example.org", &open.url)]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("ProbeTableDrift"), "{err}");
    assert!(err.contains("docs.example.org") && err.contains("missing from the descriptor set"), "{err}");
    assert!(err.contains("admin.example.org") && err.contains("missing from the probe table"), "{err}");
    // The descriptor's gate and the table's differ for one hostname: each side misses the other's pair.
    assert!(err.contains("store.example.org (public)") && err.contains("store.example.org (access)"), "{err}");
    assert!(open.requests().is_empty(), "a host was probed past the drift: {:?}", open.requests());
}

/// A descriptor decodes with excess properties refused; an unmodelled key raises `DescriptorUnknownField`, naming the key and the contract version.
// spec: topology.publish-hostname.unknown-field@9c2bdaa6
#[test]
fn a_descriptor_carrying_an_unmodelled_key_is_refused() {
    let r = Repo::init();
    let open = Host::answering(200);
    declare(&r, &[("store.example.org", "public")]);
    r.write("deploy/hostnames/store.example.org.toml", &format!("{}cache_ttl = 60\n", descriptor("store.example.org", "public")));
    let o = probe(&r, &[("store.example.org", &open.url)]);
    assert!(!o.status.success());
    let err = stderr(&o);
    assert!(err.contains("DescriptorUnknownField"), "{err}");
    assert!(err.contains("`cache_ttl`") && err.contains("version 1") && err.contains("store.example.org.toml"), "{err}");
    assert!(open.requests().is_empty());
}
