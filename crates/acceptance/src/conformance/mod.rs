//! Versioned consumer evidence over a supplied artifact's public surfaces.

mod remote;
mod scenarios;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct Manifest {
    suite_revision: String,
    scenarios: Vec<Requirement>,
}
#[derive(Deserialize)]
struct Requirement {
    id: String,
    requirement: String,
}
fn manifest() -> &'static Manifest {
    static MANIFEST: std::sync::OnceLock<Manifest> = std::sync::OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!("manifest.json")).expect("suite manifest is valid")
    })
}
pub fn suite_revision() -> &'static str {
    &manifest().suite_revision
}

#[derive(Debug, Clone)]
pub struct Options {
    pub binary: PathBuf,
    pub storage_profile: String,
    /// Empty requires every scenario in this suite revision.
    pub required: Vec<String>,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Unsupported,
    Skipped,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub required: bool,
    pub outcome: Outcome,
    pub requirement: Option<String>,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub suite_revision: String,
    pub suite_version: String,
    pub artifact_digest: String,
    pub reported_version: Option<String>,
    pub feature_profile: Option<String>,
    pub storage_profile: String,
    /// A reported version does not establish correspondence with a source commit.
    pub source_commit: Option<String>,
    pub scenarios: Vec<Scenario>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.scenarios.iter().any(|s| s.required)
            && self
                .scenarios
                .iter()
                .filter(|s| s.required)
                .all(|s| s.outcome == Outcome::Passed)
            && !self.scenarios.iter().any(|s| s.outcome == Outcome::Failed)
    }
}

type Result<T> = std::result::Result<T, String>;

pub fn digest(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).map_err(|_| "ConformanceArtifactUnavailable".to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| "ConformanceArtifactUnreadable".to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// The local and PostgreSQL profiles use synthetic fixtures; D1 requires explicit bindings.
pub fn run(options: &Options) -> Result<Report> {
    if options.timeout_seconds == 0 {
        return Err("ConformanceTimeoutInvalid".into());
    }
    let binary = options
        .binary
        .canonicalize()
        .map_err(|_| "ConformanceArtifactUnavailable".to_string())?;
    let mut report = Report {
        schema_version: 1,
        suite_revision: suite_revision().into(),
        suite_version: env!("CARGO_PKG_VERSION").into(),
        artifact_digest: digest(&binary)?,
        reported_version: None,
        feature_profile: None,
        storage_profile: options.storage_profile.clone(),
        source_commit: None,
        scenarios: vec![],
    };
    let probe = Fixture::new(&binary, options.timeout_seconds)?;
    let version = probe.invoke(&["--version"], &[]);
    let version_error = version.as_ref().err().cloned();
    if let Ok(version) = version {
        let text = version.stdout.trim();
        if version.status.success() && text.starts_with("contextful ") && text.lines().count() == 1
        {
            report.reported_version = Some(text.into());
            report.feature_profile = text.split_whitespace().nth(2).map(str::to_owned);
        }
    }
    for name in &options.required {
        if !manifest()
            .scenarios
            .iter()
            .any(|scenario| &scenario.id == name)
        {
            report.scenarios.push(Scenario {
                id: name.clone(),
                required: true,
                outcome: Outcome::Unsupported,
                requirement: Some("suite scenario".into()),
                diagnostic: Some("ConformanceScenarioUnknown".into()),
            });
        }
    }
    for scenario in &manifest().scenarios {
        let (id, requirement) = (scenario.id.as_str(), scenario.requirement.as_str());
        let required =
            options.required.is_empty() || options.required.iter().any(|name| name == id);
        let mut result = Scenario {
            id: id.into(),
            required,
            outcome: Outcome::Skipped,
            requirement: Some(requirement.into()),
            diagnostic: None,
        };
        if !required {
            report.scenarios.push(result);
            continue;
        }
        if !matches!(options.storage_profile.as_str(), "local" | "postgres" | "cloudflare-d1") {
            result.outcome = Outcome::Unsupported;
            result.requirement = Some(format!("storage profile {}", options.storage_profile));
            result.diagnostic = Some("ConformanceStorageUnsupported".into());
        } else if report.reported_version.is_none() {
            result.outcome = Outcome::Failed;
            result.diagnostic = Some(
                version_error
                    .clone()
                    .unwrap_or_else(|| "ConformanceVersionUnavailable".into()),
            );
        } else {
            let mut unavailable = None;
            let mut probe_error = None;
            for capability in requirement
                .split(',')
                .filter(|capability| *capability != "exec")
                .chain((options.storage_profile != "local").then_some("storage"))
            {
                match probe.invoke(&[capability, "--help"], &[]) {
                    Ok(checked) if checked.status.success() => {}
                    Ok(_) => {
                        unavailable = Some(capability.to_string());
                        break;
                    }
                    Err(error) => {
                        probe_error = Some(error);
                        break;
                    }
                }
            }
            if let Some(error) = probe_error {
                result.outcome = Outcome::Failed;
                result.diagnostic = Some(error);
            } else if let Some(capability) = unavailable {
                result.outcome = Outcome::Unsupported;
                result.requirement = Some(capability);
                result.diagnostic = Some("ConformanceCapabilityMissing".into());
            } else {
                let verdict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut fixture = Fixture::new(&binary, options.timeout_seconds)?;
                    if options.storage_profile != "local" {
                        fixture.configure_remote(&options.storage_profile)?;
                    }
                    scenarios::run(id, &fixture)
                }));
                match verdict {
                    Ok(Ok(())) => result.outcome = Outcome::Passed,
                    Ok(Err(error)) => {
                        result.outcome = if error.starts_with("ConformanceCapabilityMissing:") {
                            Outcome::Unsupported
                        } else {
                            Outcome::Failed
                        };
                        if result.outcome == Outcome::Unsupported {
                            result.requirement = error
                                .split_once(':')
                                .map(|(_, requirement)| requirement.into());
                        }
                        result.diagnostic = Some(error);
                    }
                    Err(_) => {
                        result.outcome = Outcome::Failed;
                        result.diagnostic = Some("ConformanceScenarioPanicked".into());
                    }
                }
            }
        }
        report.scenarios.push(result);
    }
    if digest(&binary).as_ref() != Ok(&report.artifact_digest) {
        report.scenarios.push(Scenario {
            id: "artifact-stability".into(),
            required: true,
            outcome: Outcome::Failed,
            requirement: Some("immutable artifact".into()),
            diagnostic: Some("ConformanceArtifactChanged".into()),
        });
    }
    Ok(report)
}

struct Fixture {
    dir: tempfile::TempDir,
    binary: PathBuf,
    timeout: Duration,
    remote: Option<remote::Remote>,
}

struct Captured {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    storage_capability_missing: bool,
}

impl Fixture {
    fn new(binary: &Path, timeout_seconds: u64) -> Result<Self> {
        let fixture = Self {
            dir: tempfile::tempdir().map_err(|_| "ConformanceScratchUnavailable")?,
            binary: binary.into(),
            timeout: Duration::from_secs(timeout_seconds),
            remote: None,
        };
        fixture.write("contextful.toml", "authoring_posture = \"per_request\"\n")?;
        fixture.write(
            ".contextful/context/research/config.toml",
            "[node]\nid = \"conformance\"\n",
        )?;
        fixture.write(
            ".contextful/issuance.toml",
            "default_audience = \"contextful://conformance\"\nmax_lifetime_secs = 3600\n",
        )?;
        Ok(fixture)
    }

    fn configure_remote(&mut self, profile: &str) -> Result<()> {
        let remote = if profile == "cloudflare-d1" { remote::Remote::start_d1(self.dir.path())? } else { remote::Remote::start(self.timeout)? };
        self.write(".contextful/context/research/config.toml", &remote.config)?;
        self.remote = Some(remote);
        let initialized = self.invoke(&["storage", "initialize", "--project", "research"], &[])?;
        if !initialized.status.success() {
            return Err(if initialized.storage_capability_missing {
                if profile == "cloudflare-d1" { "ConformanceCapabilityMissing:cloudflare-d1" } else { "ConformanceCapabilityMissing:postgres" }
            } else {
                "ConformanceStorageInitializeFailed"
            }
            .into());
        }
        let checked = self.ok(&["storage", "check", "--project", "research"], &[])?;
        let checked: serde_json::Value =
            serde_json::from_str(&checked).map_err(|_| "ConformanceStorageReportMalformed")?;
        if checked["replacement_safe"] != true {
            return Err("ConformanceCapabilityMissing:replacement-safe-storage".into());
        }
        self.remote.as_ref().expect("remote is configured").mark_disposable()?;
        Ok(())
    }

    fn replace_local_state(&self) -> Result<()> {
        let Some(remote) = &self.remote else {
            return Ok(());
        };
        let store = self.dir.path().join(".contextful/context/research");
        std::fs::remove_dir_all(store).map_err(|_| "ConformanceReplacementFailed")?;
        let runs = self.dir.path().join(".contextful/run");
        if runs.exists() {
            std::fs::remove_dir_all(runs).map_err(|_| "ConformanceReplacementFailed")?;
        }
        self.write(".contextful/context/research/config.toml", &remote.config)
    }

    fn write(&self, path: &str, content: &str) -> Result<()> {
        let target = self.dir.path().join(path);
        std::fs::create_dir_all(target.parent().unwrap())
            .map_err(|_| "ConformanceScratchUnavailable")?;
        std::fs::write(target, content).map_err(|_| "ConformanceScratchUnavailable".into())
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .current_dir(self.dir.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path())
            .env("TMPDIR", self.dir.path())
            .envs(
                self.remote
                    .iter()
                    .flat_map(|remote| remote.environment.iter().map(|(key, value)| (key, value))),
            )
            .envs(env.iter().copied());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command
    }

    fn invoke(&self, args: &[&str], env: &[(&str, &str)]) -> Result<Captured> {
        let mut child = Running::spawn(self.command(args, env))?;
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if child.output_limit_exceeded()? {
                return Err("ConformanceOutputLimit".into());
            }
            if let Some(status) = child
                .child
                .try_wait()
                .map_err(|_| "ConformanceProcessUnavailable")?
            {
                break status;
            }
            if Instant::now() >= deadline {
                return Err("ConformanceProcessTimeout".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let stdout = child.stdout()?;
        let stderr = child.stderr()?;
        let storage_capability_missing = !status.success() && child.storage_capability_missing()?;
        Ok(Captured {
            status,
            stdout,
            stderr,
            storage_capability_missing,
        })
    }

    fn ok(&self, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
        let out = self.invoke(args, env)?;
        if !out.status.success() {
            let verb = args.first().copied().unwrap_or("version");
            // Only the first error identifier leaves stderr; message text may carry credentials.
            return Err(match error_identifier(&out.stderr) {
                Some(code) => format!("ConformanceCommandFailed:{verb}:{code}"),
                None => format!("ConformanceCommandFailed:{verb}"),
            });
        }
        Ok(out.stdout.trim().into())
    }

    fn query(&self, sql: &str) -> Result<serde_json::Value> {
        serde_json::from_str(&self.ok(&["query", "--json", "--project", "research", sql], &[])?)
            .map_err(|_| "ConformanceQueryMalformed".into())
    }
}

/// The first `UpperCamelCase:` word of a failed command's stderr, at most 64 characters.
fn error_identifier(stderr: &str) -> Option<&str> {
    stderr.split_ascii_whitespace().find_map(|word| {
        let code = word.strip_suffix(':')?;
        let camel = code.starts_with(|c: char| c.is_ascii_uppercase())
            && code.len() <= 64
            && code.chars().all(|c| c.is_ascii_alphanumeric())
            && code.chars().any(|c| c.is_ascii_lowercase())
            && code.chars().skip(1).any(|c| c.is_ascii_uppercase());
        camel.then_some(code)
    })
}

struct Running {
    child: Child,
    out: tempfile::NamedTempFile,
    err: tempfile::NamedTempFile,
}

impl Running {
    fn spawn(mut command: Command) -> Result<Self> {
        let out = tempfile::NamedTempFile::new().map_err(|_| "ConformanceScratchUnavailable")?;
        let err = tempfile::NamedTempFile::new().map_err(|_| "ConformanceScratchUnavailable")?;
        let file = out.reopen().map_err(|_| "ConformanceScratchUnavailable")?;
        let error = err.reopen().map_err(|_| "ConformanceScratchUnavailable")?;
        let child = command
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(error))
            .spawn()
            .map_err(|_| "ConformanceProcessUnavailable")?;
        Ok(Self { child, out, err })
    }

    fn output_limit_exceeded(&self) -> Result<bool> {
        let mut bytes = 0;
        for file in [&self.out, &self.err] {
            bytes += file
                .as_file()
                .metadata()
                .map_err(|_| "ConformanceOutputUnavailable")?
                .len();
        }
        Ok(bytes > 1024 * 1024)
    }

    fn storage_capability_missing(&self) -> Result<bool> {
        let bytes = std::fs::read(self.err.path()).map_err(|_| "ConformanceOutputUnavailable")?;
        Ok(String::from_utf8_lossy(&bytes)
            .split_ascii_whitespace()
            .any(|word| {
                matches!(
                    word,
                    "ProfileCapabilityAbsent:"
                        | "StorageCapabilityMissing:"
                        | "StorageProfileUnsupported:"
                )
            }))
    }

    fn stderr(&self) -> Result<String> {
        let bytes = std::fs::read(self.err.path()).map_err(|_| "ConformanceOutputUnavailable")?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn stdout(&self) -> Result<String> {
        if self.output_limit_exceeded()? {
            return Err("ConformanceOutputLimit".into());
        }
        std::fs::read_to_string(self.out.path()).map_err(|_| "ConformanceOutputUnavailable".into())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &format!("-{}", self.child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
