use contextful_acceptance::container_recovery::docker_fixture::SyntheticFixture;
use std::time::Duration;

#[test]
fn container_fixture_requires_pinned_service_images_before_starting_services() {
    let error = SyntheticFixture::start(
        "postgres:16-alpine",
        "alpine/socat:latest",
        Duration::from_secs(1),
    )
    .err()
    .expect("mutable fixture images must be refused");
    assert_eq!(error, "ConformanceImageDigestRequired");
}

#[test]
#[ignore = "requires cached synthetic service images"]
fn synthetic_container_provider_is_reachable_on_the_shared_loopback() {
    use contextful_acceptance::container_recovery::RecoveryFixture;
    let postgres = std::env::var("CONTEXTFUL_FIXTURE_POSTGRES_IMAGE").unwrap();
    let proxy = std::env::var("CONTEXTFUL_FIXTURE_PROXY_IMAGE").unwrap();
    let fixture = SyntheticFixture::start(&postgres, &proxy, Duration::from_secs(30)).unwrap();
    let response = std::process::Command::new("docker")
        .args([
            "exec",
            fixture.network_container(),
            "wget",
            "--timeout=3",
            "-q",
            "-O",
            "-",
            "http://127.0.0.1:18080/health",
        ])
        .output()
        .unwrap();
    assert!(
        response.status.success(),
        "loopback provider unavailable: {}",
        String::from_utf8_lossy(&response.stderr)
    );
    assert!(
        response.stdout == b"ready",
        "provider health response differs"
    );
    assert_eq!(fixture.first_page_calls(), 0);
    assert_eq!(
        fixture.verification().1,
        serde_json::json!([["a", "2097152"], ["b", "4"]])
    );
}

#[test]
#[ignore = "requires a supplied digest-pinned release image and cached synthetic service images"]
fn digest_pinned_release_image_recovers_paid_pull_on_empty_disk() {
    let image =
        std::env::var("CONTEXTFUL_RECOVERY_IMAGE").expect("supplied digest-pinned release image");
    let postgres = std::env::var("CONTEXTFUL_FIXTURE_POSTGRES_IMAGE")
        .expect("pinned PostgreSQL fixture image");
    let proxy = std::env::var("CONTEXTFUL_FIXTURE_PROXY_IMAGE")
        .expect("pinned loopback proxy fixture image");
    let fixture = SyntheticFixture::start(&postgres, &proxy, Duration::from_secs(90)).unwrap();
    let evidence =
        contextful_acceptance::container_recovery::run(&image, &fixture, Duration::from_secs(120))
            .unwrap();
    assert!(evidence.hard_kill_observed && evidence.replacement_disk_empty);
    assert_eq!(evidence.first_page_calls, 1);
    println!("{}", serde_json::to_string_pretty(&evidence).unwrap());
    if let Ok(path) = std::env::var("CONTEXTFUL_RECOVERY_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "requires a supplied digest-pinned release image and cached synthetic service images"]
fn digest_pinned_release_image_retries_committed_export_after_lost_ack_and_empty_disk() {
    use contextful_acceptance::container_recovery::docker_fixture::SyntheticExportFixture;
    let image = std::env::var("CONTEXTFUL_RECOVERY_IMAGE").unwrap();
    let postgres = std::env::var("CONTEXTFUL_FIXTURE_POSTGRES_IMAGE").unwrap();
    let proxy = std::env::var("CONTEXTFUL_FIXTURE_PROXY_IMAGE").unwrap();
    let fixture =
        SyntheticExportFixture::start(&postgres, &proxy, Duration::from_secs(90)).unwrap();
    let evidence = contextful_acceptance::container_recovery::run_export(
        &image,
        &fixture,
        Duration::from_secs(120),
    )
    .unwrap();
    assert!(evidence.hard_kill_observed && evidence.replacement_disk_empty);
    assert!(evidence.retried_event_ids.len() >= 3);
    println!("{}", serde_json::to_string_pretty(&evidence).unwrap());
    if let Ok(path) = std::env::var("CONTEXTFUL_EXPORT_RECOVERY_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}
