//! `authority.attenuate`, chain side: offline derivation, truncation and per-sub-agent binding.

use crate::support::*;
use base64::Engine;
use contextful_core::grant::{Action, TablePattern};
use contextful_policy::attenuate::{attenuate, Derivation};
use contextful_policy::issue::{mint, MintClaims};
use contextful_policy::possession::{jwk_thumbprint, sign_proof, verify_proof, NonceCache, ProofRefusal, ProofRequest};
use contextful_policy::verify::{introspect, verify_with_proof, Admission};

fn tables(names: &[&str]) -> Vec<TablePattern> {
    names.iter().map(|t| TablePattern::parse(t).unwrap()).collect()
}

fn narrowed(parent: &str, to: &[&str]) -> Result<String, contextful_core::AuthorityError> {
    let grants = introspect(parent).unwrap().authority.grants;
    attenuate(parent, &Derivation::narrowing(&grants, None, Some(&tables(to))))
}

/// A holder derives a narrower child with no issuer round trip by appending a signed block. The parent's bytes stay unchanged, independently verifiable and independently withdrawable.
// spec: authority.attenuate.offline@7aa68118
#[test]
fn a_holder_derives_a_child_offline_and_the_parent_stays_intact() {
    let signer = issuer();
    let parent = minted(&signer);
    let before = parent.clone();
    // The holder holds only the credential: no key, no issuer.
    let child = narrowed(&parent, &["research/filings"]).unwrap();
    assert_eq!(parent, before);
    let admitted = admit(&child, &signer, DURING).unwrap();
    assert_eq!(admitted.grants()[0].tables, tables(&["research/filings"]));
    // The parent verifies on its own, and withdrawing the child leaves it admitted.
    let child_id = introspect(&child).unwrap().rev_id;
    refused(admit_denying(&child, &signer, &[&child_id]), "AuthorityRevoked");
    assert_eq!(admit_denying(&parent, &signer, &[&child_id]).unwrap().grants()[0].tables, tables(&["research/*"]));
    // Withdrawing the parent withdraws the child with it.
    let parent_id = introspect(&parent).unwrap().rev_id;
    refused(admit_denying(&child, &signer, &[&parent_id]), "AuthorityRevoked");
    // The holder's own check refuses a widening before any block is signed.
    refused(narrowed(&parent, &["sales/*"]), "AttenuationWidens");
    let grants = introspect(&parent).unwrap().authority.grants;
    refused(attenuate(&parent, &Derivation::narrowing(&grants, Some(&[Action::Write]), None)), "AttenuationWidens");
    let later = Derivation { expires_at: Some(at("2031-01-01T00:00:00Z")), ..Derivation::default() };
    refused(attenuate(&parent, &later), "AttenuationExpiryExtended");
}

/// The chain-final proof advances an ephemeral key per block; a chain truncated to a broader prefix verifies as nothing.
// spec: authority.attenuate.truncation@149b4bd8
#[test]
fn a_chain_truncated_to_a_broader_prefix_verifies_as_nothing() {
    let signer = issuer();
    let parent = minted(&signer);
    let child = narrowed(&parent, &["research/filings"]).unwrap();
    let grandchild = narrowed(&child, &["research/filings/eu"]).unwrap();
    assert!(admit(&grandchild, &signer, DURING).is_ok());
    let key = keys(&signer).keys().next().unwrap().public_key;
    let token = biscuit_auth::Biscuit::from_base64(&grandchild, key).unwrap();
    let mut container = token.container().clone();
    // Drop the last block and keep the chain-final proof: the proof's key belongs to the
    // dropped block, so the prefix verifies as nothing.
    container.blocks.pop();
    let truncated = base64::engine::general_purpose::URL_SAFE.encode(container.to_vec().unwrap());
    refused(admit(&truncated, &signer, DURING), "SignatureInvalid");
}

/// An agent derives one child per sub-agent, each bound by its confirmation claim to that sub-agent's own key pair.
// spec: authority.attenuate.per-sub-agent@d736341d
#[test]
fn each_sub_agent_child_binds_that_sub_agents_own_key() {
    let signer = issuer();
    let agent = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let agent_jkt = jwk_thumbprint(agent.verifying_key().as_bytes());
    let parent = mint(&plan(&signer), &MintClaims { confirmation: Some(agent_jkt.clone()), epoch: 0 }, &signer).unwrap();
    let sub_agents = [ed25519_dalek::SigningKey::from_bytes(&[2; 32]), ed25519_dalek::SigningKey::from_bytes(&[3; 32])];
    let request = ProofRequest { method: "POST", target: "https://store.example/v1/query", body: b"{}" };
    let clock = contextful_core::ports::FixedClock(at(DURING));
    let mut nonces = NonceCache::new();
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation).expecting(AUD);
    let ks = keys(&signer);
    for (i, sub) in sub_agents.iter().enumerate() {
        let jkt = jwk_thumbprint(sub.verifying_key().as_bytes());
        let child = attenuate(&parent, &Derivation { confirmation: Some(jkt.clone()), ..Derivation::default() }).unwrap();
        // The child's own key proves possession of it.
        let proof = sign_proof(sub, &request, at(DURING), &format!("own-{i}"));
        let admitted =
            verify_with_proof(&child, &ks, &admission, |cnf| verify_proof(cnf, &proof, &request, &clock, &mut nonces)).unwrap();
        assert_eq!(admitted.confirmation(), Some(jkt.as_str()));
        // Neither the agent's key nor a sibling's does.
        for (name, other) in [("agent", &agent), ("sibling", &sub_agents[1 - i])] {
            let proof = sign_proof(other, &request, at(DURING), &format!("{name}-{i}"));
            let r = verify_with_proof(&child, &ks, &admission, |cnf| verify_proof(cnf, &proof, &request, &clock, &mut nonces));
            assert!(
                matches!(r, Err(ProofRefusal::Refused(contextful_core::AuthorityError::PossessionProofInvalid(_)))),
                "{name}: {r:?}"
            );
        }
    }
}

#[test]
fn a_credential_binding_no_key_refuses_a_possession_check() {
    let signer = issuer();
    let revocation = no_revocation();
    let admission = Admission::new(at(DURING), &revocation);
    let r = verify_with_proof(&minted(&signer), &keys(&signer), &admission, |_| Ok::<(), ProofRefusal>(()));
    assert!(matches!(r, Err(ProofRefusal::Refused(contextful_core::AuthorityError::PossessionProofInvalid(_)))), "{r:?}");
}
