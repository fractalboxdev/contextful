use contextful_acceptance::container_recovery::{
    self, docker_fixture::SyntheticFixture, RecoveryFixture,
};
use contextful_core::run::journal::{Row, Stored};
use contextful_core::run::ports::JournalStore;
use contextful_core::time::Instant;
use contextful_engine::awake::{Awaited, Registry};
use contextful_engine::journal::Journal;
use contextful_pg::PgRunStores;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

struct PendingFixture {
    base: SyntheticFixture,
    token: Mutex<Option<String>>,
}

fn now() -> Instant {
    Instant::from_unix_nanos(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i128,
    )
    .unwrap()
}

impl RecoveryFixture for PendingFixture {
    fn network_container(&self) -> &str {
        self.base.network_container()
    }
    fn declaration(&self) -> String {
        self.base.declaration()
    }
    fn corpus_config(&self) -> String {
        self.base.corpus_config()
    }
    fn environment(&self) -> BTreeMap<String, String> {
        self.base.environment()
    }
    fn wait_for_second_page(&self, timeout: Duration) -> Result<(), String> {
        self.base.wait_for_second_page(timeout)?;
        let stores = PgRunStores::connect(&self.base.host_catalog_url(timeout)?)
            .map_err(|error| error.to_string())?;
        let registry = Registry::over(
            stores.awakeables,
            Journal::over(stores.journal, stores.blobs),
        );
        let row = registry
            .suspend(
                "synthetic-pending-callback",
                "approval",
                &now().to_string(),
                3600,
            )
            .map_err(|error| format!("{error:?}"))?;
        *self.token.lock().unwrap() = Some(row.token);
        Ok(())
    }
    fn release_second_page(&self) -> Result<(), String> {
        self.base.release_second_page()
    }
    fn first_page_calls(&self) -> usize {
        self.base.first_page_calls()
    }
    fn verification(&self) -> (String, serde_json::Value) {
        self.base.verification()
    }
}

#[test]
#[ignore = "requires supplied digest-pinned release image and cached synthetic Docker services"]
fn pending_awakeable_and_large_callback_survive_release_image_replacement() {
    let image = std::env::var("CONTEXTFUL_RECOVERY_IMAGE").unwrap();
    let postgres = std::env::var("CONTEXTFUL_FIXTURE_POSTGRES_IMAGE").unwrap();
    let proxy = std::env::var("CONTEXTFUL_FIXTURE_PROXY_IMAGE").unwrap();
    let fixture = PendingFixture {
        base: SyntheticFixture::start(&postgres, &proxy, Duration::from_secs(90)).unwrap(),
        token: Mutex::new(None),
    };
    let evidence = container_recovery::run(&image, &fixture, Duration::from_secs(120)).unwrap();
    assert!(evidence.hard_kill_observed && evidence.replacement_disk_empty);
    let token = fixture.token.lock().unwrap().clone().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    container_recovery::prepare_disk(replacement.path(), &fixture).unwrap();
    assert!(!replacement.path().join(".contextful/run").exists());
    assert!(!replacement
        .path()
        .join(".contextful/context/research/machine.sqlite")
        .exists());
    let payload =
        serde_json::to_vec(&serde_json::json!({"answer":"callback".repeat(300_000)})).unwrap();
    assert!(payload.len() > 1024 * 1024);
    std::fs::write(replacement.path().join("callback.json"), &payload).unwrap();
    let invoke = |args: &[&str]| {
        let mut command = Command::new("docker");
        command.args([
            "run",
            "--rm",
            "--network",
            &format!("container:{}", fixture.network_container()),
            "--mount",
            &format!("type=bind,src={},dst=/data", replacement.path().display()),
        ]);
        for (key, value) in fixture.environment().into_iter().chain([(
            "CONTEXTFUL_STORAGE_PROFILE".into(),
            "postgres".into(),
        )]) {
            command.args(["--env", &format!("{key}={value}")]);
        }
        let output = command.arg(&image).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let pending: serde_json::Value = serde_json::from_slice(&invoke(&[
        "run",
        "awakeable",
        &token,
        "--project",
        "research",
    ]))
    .unwrap();
    assert_eq!(pending["state"], "pending");
    let resolved = invoke(&[
        "run",
        "awake",
        &token,
        "--project",
        "research",
        "--payload",
        "callback.json",
    ]);
    assert_eq!(resolved, payload);
    let stores = PgRunStores::connect(
        &fixture
            .base
            .host_catalog_url(Duration::from_secs(10))
            .unwrap(),
    )
    .unwrap();
    let key = contextful_engine::awake::resume_key("synthetic-pending-callback", &token);
    assert!(
        matches!(stores.journal.read(&key).unwrap(), Some(Row::Recorded { value: Stored::Blob { bytes, .. }, .. }) if bytes > 1024 * 1024)
    );
    let registry = Registry::over(
        stores.awakeables,
        Journal::over(stores.journal, stores.blobs),
    );
    assert_eq!(
        registry
            .awaited("synthetic-pending-callback", &token, now())
            .unwrap(),
        Awaited::Resumed(payload.clone())
    );
    let state: serde_json::Value = serde_json::from_slice(&invoke(&[
        "run",
        "awakeable",
        &token,
        "--project",
        "research",
    ]))
    .unwrap();
    assert_eq!(state["state"], "resolved");
    if let Ok(path) = std::env::var("CONTEXTFUL_AWAKEABLE_RECOVERY_EVIDENCE") {
        let report = serde_json::json!({"image_digest":evidence.image_digest,"hard_kill_observed":true,"replacement_disk_empty":true,"pending_after_replacement":true,"callback_bytes":payload.len(),"callback_resume_blob":true});
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
