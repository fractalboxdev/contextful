//! A supplied release image resumes a recorded HTTP pull on an empty replacement disk.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub mod docker_fixture;

type Result<T> = std::result::Result<T, String>;

/// External synthetic services occupy the named container's network namespace. The
/// provider blocks its second page after observing the first page's completed pull.
pub trait RecoveryFixture {
    fn network_container(&self) -> &str;
    fn declaration(&self) -> String;
    fn corpus_config(&self) -> String;
    /// Includes the synthetic database, recovery identity and bucket credential bindings.
    fn environment(&self) -> BTreeMap<String, String>;
    fn wait_for_second_page(&self, timeout: Duration) -> Result<()>;
    fn release_second_page(&self) -> Result<()>;
    fn first_page_calls(&self) -> usize;
    /// A query and expected rows over the replacement's landed table.
    fn verification(&self) -> (String, serde_json::Value);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryEvidence {
    pub image_digest: String,
    pub image_id: String,
    pub storage_profile: String,
    pub hard_kill_observed: bool,
    pub replacement_disk_empty: bool,
    pub first_page_calls: usize,
    pub rows: serde_json::Value,
}

/// A mutable tag supplies no release-artifact evidence.
pub fn validate_image(image: &str) -> Result<()> {
    let Some((name, digest)) = image.split_once("@sha256:") else {
        return Err("ConformanceImageDigestRequired".into());
    };
    if name.is_empty()
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"./:_-".contains(&c))
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("ConformanceImageDigestRequired".into());
    }
    Ok(())
}

struct Invocation {
    child: Child,
    output: tempfile::NamedTempFile,
    diagnostic: tempfile::NamedTempFile,
}

impl Invocation {
    fn spawn(mut command: Command) -> Result<Self> {
        let output = tempfile::NamedTempFile::new().map_err(|_| "ConformanceScratchUnavailable")?;
        let diagnostic =
            tempfile::NamedTempFile::new().map_err(|_| "ConformanceScratchUnavailable")?;
        let child = command
            .stdout(Stdio::from(
                output
                    .reopen()
                    .map_err(|_| "ConformanceScratchUnavailable")?,
            ))
            .stderr(Stdio::from(
                diagnostic
                    .reopen()
                    .map_err(|_| "ConformanceScratchUnavailable")?,
            ))
            .spawn()
            .map_err(|_| "ConformanceDockerUnavailable")?;
        Ok(Self {
            child,
            output,
            diagnostic,
        })
    }
    fn wait(&mut self, timeout: Duration) -> Result<(ExitStatus, String)> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|_| "ConformanceProcessUnavailable")?
            {
                if !status.success()
                    && std::env::var_os("CONTEXTFUL_RECOVERY_DIAGNOSTICS").is_some()
                {
                    use std::io::Read;
                    let mut detail = String::new();
                    let _ = self
                        .diagnostic
                        .reopen()
                        .map(|file| file.take(16 * 1024).read_to_string(&mut detail));
                    eprintln!("container recovery process {status}: {detail}");
                }
                if self
                    .output
                    .as_file()
                    .metadata()
                    .map_err(|_| "ConformanceOutputUnavailable")?
                    .len()
                    > 1024 * 1024
                {
                    return Err("ConformanceOutputLimit".into());
                }
                return Ok((
                    status,
                    std::fs::read_to_string(self.output.path())
                        .map_err(|_| "ConformanceOutputUnavailable")?,
                ));
            }
            if Instant::now() >= deadline {
                return Err("ConformanceProcessTimeout".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Container(String);
impl Drop for Container {
    fn drop(&mut self) {
        let mut command = Command::new("docker");
        command.args(["rm", "--force", &self.0]);
        if let Ok(mut process) = Invocation::spawn(command) {
            let _ = process.wait(Duration::from_secs(10));
        }
    }
}

fn docker(args: &[&str], timeout: Duration) -> Result<String> {
    let mut command = Command::new("docker");
    command.args(args);
    let (status, out) = Invocation::spawn(command)?.wait(timeout)?;
    if !status.success() {
        return Err("ConformanceDockerCommandFailed".into());
    }
    Ok(out.trim().into())
}

fn config(dir: &Path, fixture: &dyn RecoveryFixture) -> Result<()> {
    let store = dir.join(".contextful/context/research");
    std::fs::create_dir_all(&store).map_err(|_| "ConformanceScratchUnavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [
            dir.to_path_buf(),
            dir.join(".contextful"),
            dir.join(".contextful/context"),
            store.clone(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
                .map_err(|_| "ConformanceScratchUnavailable")?;
        }
    }
    std::fs::write(dir.join("contextful.toml"), fixture.declaration())
        .map_err(|_| "ConformanceScratchUnavailable")?;
    std::fs::write(store.join("config.toml"), fixture.corpus_config())
        .map_err(|_| "ConformanceScratchUnavailable")?;
    Ok(())
}

fn command(
    image: &str,
    network: &str,
    disk: &Path,
    env: &Path,
    name: &str,
    args: &[&str],
) -> Command {
    let mut command = Command::new("docker");
    command
        .args([
            "run",
            "--rm",
            "--name",
            name,
            "--network",
            &format!("container:{network}"),
            "--env-file",
        ])
        .arg(env)
        .arg("--mount")
        .arg(format!("type=bind,src={},dst=/data", disk.display()))
        .arg(image)
        .args(args);
    command
}

fn invoke(
    image: &str,
    fixture: &dyn RecoveryFixture,
    disk: &Path,
    env: &Path,
    name: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(ExitStatus, String)> {
    let _container = Container(name.into());
    Invocation::spawn(command(
        image,
        fixture.network_container(),
        disk,
        env,
        name,
        args,
    ))?
    .wait(timeout)
}

/// Fixture services are synthetic and separately owned; missing images never count as a pass.
pub fn run(
    image: &str,
    fixture: &dyn RecoveryFixture,
    timeout: Duration,
) -> Result<RecoveryEvidence> {
    validate_image(image)?;
    if timeout.is_zero() || fixture.network_container().is_empty() {
        return Err("ConformanceFixtureInvalid".into());
    }
    let image_id = docker(&["image", "inspect", image, "--format", "{{.Id}}"], timeout)
        .map_err(|_| "ConformanceImageUnavailable".to_string())?;
    let root = tempfile::tempdir().map_err(|_| "ConformanceScratchUnavailable")?;
    let old_disk = root.path().join("old");
    let replacement = root.path().join("replacement");
    config(&old_disk, fixture)?;
    config(&replacement, fixture)?;
    let empty = !replacement.join(".contextful/run").exists()
        && !replacement
            .join(".contextful/context/research/machine.sqlite")
            .exists()
        && !replacement
            .join(".contextful/context/research/tables")
            .exists();
    if !empty {
        return Err("ConformanceReplacementDiskNotEmpty".into());
    }
    let mut environment = fixture.environment();
    environment.insert(
        "CONTEXTFUL_STORAGE_PROFILE".into(),
        "cloudflare-postgres".into(),
    );
    for key in ["CONTEXTFUL_CATALOG_URL", "CONTEXTFUL_RECOVERY_ID"] {
        if environment.get(key).is_none_or(|v| v.is_empty()) {
            return Err(format!("ConformanceCapabilityMissing:{key}"));
        }
    }
    if environment
        .iter()
        .any(|(k, v)| k.is_empty() || k.contains(['=', '\n', '\r']) || v.contains(['\n', '\r']))
    {
        return Err("ConformanceFixtureEnvironmentInvalid".into());
    }
    let env = root.path().join("bindings.env");
    std::fs::write(
        &env,
        environment
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect::<String>(),
    )
    .map_err(|_| "ConformanceScratchUnavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "ConformanceScratchUnavailable")?;
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = format!(
        "contextful-recovery-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let init = invoke(
        image,
        fixture,
        &old_disk,
        &env,
        &format!("{name}-initialize"),
        &["storage", "initialize", "--project", "research"],
        timeout,
    )?;
    if !init.0.success() {
        return Err("ConformanceStorageInitializationFailed".into());
    }
    let check = invoke(
        image,
        fixture,
        &old_disk,
        &env,
        &format!("{name}-check"),
        &["storage", "check", "--project", "research"],
        timeout,
    )?;
    let report: serde_json::Value =
        serde_json::from_str(&check.1).map_err(|_| "ConformanceStorageReportMalformed")?;
    if !check.0.success() || report["replacement_safe"] != true {
        return Err("ConformanceStorageUnsupported".into());
    }
    let old = Container(format!("{name}-old"));
    let mut running = Invocation::spawn(command(
        image,
        fixture.network_container(),
        &old_disk,
        &env,
        &old.0,
        &[
            "pipeline",
            "run",
            "feed",
            "--project",
            "research",
            "--run-id",
            "image-crash-1",
            "--site-id",
            "conformance",
        ],
    ))?;
    fixture.wait_for_second_page(timeout)?;
    if fixture.first_page_calls() != 1 {
        return Err("ConformanceRecordedCallCountMismatch".into());
    }
    docker(&["kill", "--signal", "KILL", &old.0], timeout)?;
    let killed = running.wait(timeout)?.0;
    if killed.code() != Some(137) {
        return Err("ConformanceHardCrashNotObserved".into());
    }
    fixture.release_second_page()?;
    let deadline = Instant::now() + timeout;
    let mut attempt = 0u64;
    loop {
        attempt += 1;
        let id = format!("image-resume-{attempt}");
        let resumed = invoke(
            image,
            fixture,
            &replacement,
            &env,
            &format!("{name}-resume-{attempt}"),
            &[
                "pipeline",
                "run",
                "feed",
                "--project",
                "research",
                "--run-id",
                &id,
                "--site-id",
                "conformance",
            ],
            timeout,
        )?;
        if resumed.0.success() {
            break;
        }
        if Instant::now() >= deadline {
            return Err("ConformanceReplacementResumeFailed".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if fixture.first_page_calls() != 1 {
        return Err("ConformanceRecordedCallRepeated".into());
    }
    let (sql, expected) = fixture.verification();
    let answer = invoke(
        image,
        fixture,
        &replacement,
        &env,
        &format!("{name}-query"),
        &["query", "--json", "--project", "research", &sql],
        timeout,
    )?;
    let answer: serde_json::Value =
        serde_json::from_str(&answer.1).map_err(|_| "ConformanceQueryMalformed")?;
    if answer["rows"] != expected {
        return Err("ConformanceReplacementRowsChanged".into());
    }
    if docker(&["image", "inspect", image, "--format", "{{.Id}}"], timeout)? != image_id {
        return Err("ConformanceArtifactChanged".into());
    }
    Ok(RecoveryEvidence {
        image_digest: image.split_once('@').expect("validated image").1.into(),
        image_id,
        storage_profile: "cloudflare-postgres".into(),
        hard_kill_observed: true,
        replacement_disk_empty: empty,
        first_page_calls: fixture.first_page_calls(),
        rows: answer["rows"].clone(),
    })
}
