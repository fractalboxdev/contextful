//! `authority.bound-redistribution`: the two refusals a push raises under the bound.

use contextful_core::enforce::EnforceError;

/// Each refusal's `Display` begins with its identifier, so a push surface names the refusal a
/// caller greps for.
#[test]
fn the_redistribution_refusals_print_their_identifiers() {
    for (e, id) in [
        (EnforceError::RootObjectNotAllowlisted("research/records/run-1.json".into()), "EnforceRootObjectNotAllowlisted"),
        (EnforceError::StaleRedistributedObjects("research/tables/filings/schema.json".into()), "EnforceStaleRedistributedObjects"),
    ] {
        assert_eq!(e.identifier(), id);
        assert!(e.to_string().starts_with(&format!("{id}: research/")), "{e}");
    }
}
