//! One release consumer flow, with an optional S3 service outside the test process.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ACCESS_KEY: &str = "AKIACONTEXTFULTEST01";
const SECRET_KEY: &str = "contextful-test-secret-access-key-0000001";
const SOURCE_ROW: &[u8] = b"{\"note_id\":\"n1\",\"title\":\"Solar battery storage\",\"embedding\":[1.0,0.0,0.0]}\n";

struct Minio {
    child: Child,
    data: PathBuf,
}

impl Drop for Minio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

fn checked(mut command: Command, step: &str) -> Result<()> {
    let status = command.status().with_context(|| format!("starting {step}"))?;
    if !status.success() {
        bail!("{step} exited with {status}");
    }
    Ok(())
}

fn aws(endpoint: &str, args: &[&str]) -> Result<()> {
    let mut command = Command::new("aws");
    command.args(["--endpoint-url", endpoint, "s3api"])
        .args(args)
        .env("AWS_ACCESS_KEY_ID", ACCESS_KEY)
        .env("AWS_SECRET_ACCESS_KEY", SECRET_KEY)
        .env("AWS_DEFAULT_REGION", "us-east-1")
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    checked(command, &format!("aws s3api {}", args.first().copied().unwrap_or("")))
}

fn start_minio() -> Result<(Minio, String)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("reserving MinIO port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let data = std::env::temp_dir().join(format!("contextful-e2e-minio-{}", std::process::id()));
    std::fs::create_dir(&data).context("creating MinIO data directory")?;
    let child = match Command::new("minio")
        .args(["server", data.to_str().context("MinIO data path is not UTF-8")?, "--address", &format!("127.0.0.1:{port}"), "--console-address", ":0"])
        .env("MINIO_ROOT_USER", ACCESS_KEY)
        .env("MINIO_ROOT_PASSWORD", SECRET_KEY)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&data);
                return Err(error).context("starting MinIO; install the `minio` server binary to use --minio");
            }
        };
    let server = Minio { child, data };
    let endpoint = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if aws(&endpoint, &["list-buckets"]).is_ok() {
            aws(&endpoint, &["create-bucket", "--bucket", "source"])?;
            aws(&endpoint, &["create-bucket", "--bucket", "shared"])?;
            return Ok((server, endpoint));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("MinIO did not answer S3 requests within 60 seconds")
}

/// Drive the standalone acceptance package, whose harness builds the release executable.
pub fn run(root: &Path, minio: bool) -> Result<()> {
    let (container, endpoint) = if minio {
        let (container, endpoint) = start_minio()?;
        let row = std::env::temp_dir().join(format!("contextful-e2e-source-{}.jsonl", std::process::id()));
        std::fs::write(&row, SOURCE_ROW).context("writing source fixture")?;
        let seeded = aws(&endpoint, &["put-object", "--bucket", "source", "--key", "notes.jsonl", "--body", row.to_str().context("source fixture path is not UTF-8")?]);
        let _ = std::fs::remove_file(&row);
        seeded?;
        (Some(container), Some(endpoint))
    } else {
        (None, None)
    };
    let mut test = Command::new("cargo");
    test.args(["test", "--locked", "-p", "contextful-acceptance", "--test", "integration", "e2e::e2e_consumer_round_trip", "--", "--exact", "--nocapture"])
        .current_dir(root)
        .env("CONTEXTFUL_ACCEPTANCE_PROFILE", "release");
    if let Some(endpoint) = &endpoint {
        test.env("CONTEXTFUL_E2E_S3_ENDPOINT", endpoint);
    }
    let output = test.output().context("starting release consumer flow")?;
    std::io::stdout().write_all(&output.stdout).context("reporting release consumer flow")?;
    std::io::stderr().write_all(&output.stderr).context("reporting release consumer flow errors")?;
    let result = if !output.status.success() {
        bail!("release consumer flow exited with {}", output.status);
    } else if !String::from_utf8_lossy(&output.stdout).lines().any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;")) {
        bail!("no consumer flow test ran");
    } else {
        Ok(())
    };
    if let Some(endpoint) = &endpoint {
        if result.is_ok() {
            aws(endpoint, &["head-object", "--bucket", "shared", "--key", "team/manifest.json"])?;
        }
    }
    drop(container);
    result
}
