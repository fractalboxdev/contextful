//! Image recovery admits only an immutable supplied release artifact.

#[test]
fn container_recovery_requires_a_digest_pinned_image_before_starting_fixtures() {
    use contextful_acceptance::container_recovery::validate_image;
    assert!(validate_image("registry.example/contextful@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_ok());
    for image in [
        "contextful:latest",
        "contextful@sha256:1234",
        "-option@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "contextful@sha256:gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
    ] {
        assert_eq!(
            validate_image(image).unwrap_err(),
            "ConformanceImageDigestRequired"
        );
    }
}
