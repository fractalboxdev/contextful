//! A finite command-source process tree for native integration tests.
use std::io::Write;
use std::time::{Duration, Instant};

fn main() {
    let mode = std::env::args().nth(1).expect("fixture mode");
    if mode == "grandchild" {
        std::fs::write("grandchild.pid", std::process::id().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    assert!(matches!(mode.as_str(), "cancel" | "finished"));
    let _child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("grandchild")
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::path::Path::new("grandchild.pid").exists() {
        assert!(Instant::now() < deadline, "grandchild readiness");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write("parent.ready", "ready").unwrap();
    if mode == "finished" {
        print!("{{\"rows\":[]}}");
        std::io::stdout().flush().unwrap();
    } else {
        std::thread::sleep(Duration::from_secs(30));
    }
}
