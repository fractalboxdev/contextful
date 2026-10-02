//! Host containment and concrete widening witnesses use the outbound matcher.
use contextful_core::connector::attach::Allowlist;

#[test]
fn inclusion_preserves_every_candidate_host() {
    let parent = Allowlist::parse(&["*.example.com", "other.net"]).unwrap();
    for entries in [vec!["api.example.com"], vec!["*.eu.example.com"], vec!["*.example.com", "other.net"], vec!["*..example.com"]] {
        let child = Allowlist::parse(&entries).unwrap();
        assert!(child.included_in(&parent));
        for host in ["example.com", "api.example.com", "x.eu.example.com", "eu.example.com", "other.net", "OTHER.NET.", "evilexample.com"] {
            assert!(!child.permits(host) || parent.permits(host));
        }
    }
    for entry in ["example.com", "*.com", "elsewhere.net"] {
        assert!(!Allowlist::parse(&[entry]).unwrap().included_in(&parent));
    }
    assert!(!Allowlist::parse(&["*.example.com"]).unwrap().included_in(&Allowlist::parse(&["x.example.com"]).unwrap()));
}

#[test]
fn widening_witness_replays_against_both_allowlists() {
    let parent = Allowlist::parse(&["x.example.com", "*.eu.example.com"]).unwrap();
    for entry in ["*.example.com", "example.com", "elsewhere.net"] {
        let child = Allowlist::parse(&[entry]).unwrap();
        let witness = child.widening_witness(&parent).expect("a newly admitted host");
        assert!(child.permits(&witness), "{witness}");
        assert!(!parent.permits(&witness), "{witness}");
    }
    assert!(parent.widening_witness(&parent).is_none());
    let collisions = Allowlist::parse(&["w0.example.com", "w1.example.com", "*.eu.example.com"]).unwrap();
    let child = Allowlist::parse(&["*.example.com"]).unwrap();
    let witness = child.widening_witness(&collisions).unwrap();
    assert!(child.permits(&witness) && !collisions.permits(&witness));
    // A trailing-dot entry is unreachable after the runtime normalizes a host.
    let unreachable = Allowlist::parse(&["api.example.com."]).unwrap();
    assert!(!unreachable.included_in(&parent));
    assert!(unreachable.widening_witness(&parent).is_none());
}

/// The executable reference imports the proved model, sharing its decision definitions.
// spec: connector.widen.host-inclusion@f6a2edfe
// spec: connector.widen.host-witness@e13c9fd4
#[test]
fn generated_hosts_agree_with_the_proved_model() {
    use contextful_core::connector::attach::HostEntry;
    use serde_json::{json, Value};
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../formal/reference");
    let out = match Command::new("lake").arg("build").current_dir(&root).output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            assert!(std::env::var_os("CONTEXTFUL_REQUIRE_LEAN").is_none(), "lake is required");
            eprintln!("skipped: lake is absent");
            return;
        }
        Err(e) => panic!("lake build: {e}"),
    };
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let exe = root.join(".lake/build/bin/contextful-reference");
    let reference = |case: Value| -> bool {
        let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(case.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(answer["error"].is_null(), "{case}: {answer}");
        answer["verdict"] == "covered"
    };
    let entries = |a: &Allowlist| -> Value {
        json!(a
            .0
            .iter()
            .map(|e| match e {
                HostEntry::Exact(v) => json!({"kind":"exact", "value":v}),
                HostEntry::Subdomains(v) => json!({"kind":"subdomains", "value":v}),
            })
            .collect::<Vec<_>>())
    };
    // Fixed seed; varying depths, case, terminal dots, exact entries and wildcard unions.
    let mut seed = 0x5eed_u64;
    for _ in 0..80 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n = (seed >> 32) % 19;
        let suffix = format!("zone{n}.example.com");
        let pool = [
            suffix.clone(),
            format!("*.{suffix}"),
            format!("api.{suffix}"),
            "*.example.com".into(),
            "w0.example.com".into(),
            "*..example.com".into(),
            "É.example.com".into(),
            "api.example.com.".into(),
            "[::1]".into(),
        ];
        let candidate = Allowlist::parse(&[&pool[(seed as usize) % pool.len()]]).unwrap();
        let predecessor =
            Allowlist::parse(&[&pool[((seed >> 8) as usize) % pool.len()], &pool[((seed >> 16) as usize) % pool.len()]]).unwrap();
        assert_eq!(
            candidate.included_in(&predecessor),
            reference(json!({"op":"host_included", "candidate":entries(&candidate), "predecessor":entries(&predecessor)}))
        );
        for host in [
            &suffix,
            &format!("API.{suffix}."),
            &format!("deep.api.{suffix}"),
            &format!("evil{suffix}"),
            "É.example.com",
            "é.example.com",
            "[::1]",
            ".example.com",
        ] {
            assert_eq!(
                candidate.permits(host),
                reference(json!({"op":"host_covers", "entries":entries(&candidate), "host":host})),
                "{candidate:?}: {host}"
            );
        }
        if let Some(host) = candidate.widening_witness(&predecessor) {
            assert!(reference(
                json!({"op":"host_witness", "candidate":entries(&candidate), "predecessor":entries(&predecessor), "host":host})
            ));
        }
    }
}
