//! `surface.reside`: the residency allow-set, the resources held against it, and the set each site records.

use contextful_core::surface::reside::{compare_sites, Residency, Resource, SiteRegions, REGION_ENTRIES};

fn set(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|e| e.to_string()).collect()
}

/// A residency allow-set holds at most 16 entries.
// spec: surface.reside.region-entries@5ed1db19
#[test]
fn an_allow_set_holds_sixteen_entries() {
    assert_eq!(REGION_ENTRIES, 16);
    let sixteen: Vec<String> = (0..16).map(|i| format!("region-{i}")).collect();
    Residency::parse(&sixteen).unwrap();
    let seventeen: Vec<String> = (0..17).map(|i| format!("region-{i}")).collect();
    let e = Residency::parse(&seventeen).unwrap_err().to_string();
    assert!(e.contains("17 entries") && e.contains("16"), "{e}");
    let e = Residency::parse(&set(&["EU West"])).unwrap_err().to_string();
    assert!(e.contains("EU West"), "{e}");
}

/// A resource resolving to a region the policy omits raises `EnforceRegionMismatch`, naming every such resource.
#[test]
fn a_resource_outside_the_allow_set_refuses_the_start() {
    let policy = Residency::parse(&set(&["eu-west-1", "eu-central-1"])).unwrap();
    let inside = Resource { name: "the `[sync]` bucket".into(), region: "eu-west-1".into() };
    policy.enforce(std::slice::from_ref(&inside)).unwrap();
    let outside = Resource { name: "pipeline `orders` source".into(), region: "us-east-1".into() };
    let e = policy.enforce(&[inside, outside]).unwrap_err();
    assert_eq!(e.status(), 503);
    let e = e.to_string();
    assert!(e.starts_with("EnforceRegionMismatch") && e.contains("pipeline `orders` source resolves to `us-east-1`"), "{e}");
    assert!(!e.contains("bucket resolves"), "{e}");
    let any = Residency::parse(&set(&["*"])).unwrap();
    any.enforce(&[Resource { name: "x".into(), region: "ap-south-1".into() }]).unwrap();
}

/// A push finding the bucket manifest record another site's differing allow-set raises `ResidencySitesDiverge`;
/// an equal set, or no record, passes.
#[test]
fn two_sites_declaring_different_sets_diverge() {
    let recorded = SiteRegions { site_id: "site-a".into(), regions: set(&["eu-west-1"]) };
    compare_sites("site-b", Some(&set(&["eu-west-1"])), Some(&recorded)).unwrap();
    compare_sites("site-b", Some(&set(&["us-east-1"])), None).unwrap();
    compare_sites("site-b", None, None).unwrap();
    for own in [Some(set(&["us-east-1"])), Some(set(&["eu-west-1", "us-east-1"])), None] {
        let e = compare_sites("site-b", own.as_deref(), Some(&recorded)).unwrap_err().to_string();
        assert!(e.starts_with("ResidencySitesDiverge") && e.contains("site-a") && e.contains("site-b"), "{e}");
    }
}
