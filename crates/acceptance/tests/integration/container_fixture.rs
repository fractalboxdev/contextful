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
