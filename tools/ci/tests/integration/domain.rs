//! `topology.package.domain-crate`: the domain crate performs no I/O and every profile
//! links it.

use crate::repo_root;
use std::path::Path;
use std::process::Command;

/// Calls reaching a file, a socket, a process, the environment, the terminal or the wall
/// clock: what an adapter does through a port and the domain crate never does itself.
const IO: [&str; 13] = [
    "std::fs",
    "std::process",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "stdin()",
    "stdout()",
    "stderr()",
    "std::env::var",
    "thread::sleep",
    "SystemTime::now",
    "println!",
    "eprintln!",
];

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `contextful-core` holds the pure domain types and the port traits every adapter implements, performs no I/O, and links into every profile.
// spec: topology.package.domain-crate
#[test]
fn the_domain_crate_performs_no_io_and_links_into_every_profile() {
    let mut files = Vec::new();
    sources(&repo_root().join("crates/contextful-core/src"), &mut files);
    assert!(!files.is_empty());
    let mut found = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for call in IO.iter().filter(|c| code.contains(*c)) {
                found.push(format!("{}:{}: {call}", f.display(), n + 1));
            }
        }
    }
    assert!(found.is_empty(), "the domain crate performs I/O:\n{}", found.join("\n"));

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    for profile in ["contextful-control", "contextful-edge", "contextful-full"] {
        let o = Command::new(&cargo)
            .args(["tree", "--locked", "-p", "contextful-cli", "--no-default-features", "--features", profile])
            .args(["-e", "normal", "--prefix", "none", "--format", "{p}"])
            .current_dir(repo_root())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let tree = String::from_utf8_lossy(&o.stdout);
        assert!(tree.lines().any(|l| l.starts_with("contextful-core ")), "`{profile}` links no domain crate");
    }
}
