//! Host executable and registered semantic identity bind replay before any input read.

use super::Drive;
use contextful_core::run::journal::sha256_hex;
use contextful_core::run::own::{OwnerPins, PlanPins};
use contextful_core::run::{Failure, FailureTag};
use std::collections::BTreeMap;
use std::sync::OnceLock;

fn host_implementation() -> Result<String, Failure> {
    static DIGEST: OnceLock<Result<String, Failure>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let executable = std::env::current_exe().map_err(|_| {
                Failure::new(
                    FailureTag::Config,
                    "host executable identity is unavailable",
                )
            })?;
            let bytes = std::fs::read(executable).map_err(|_| {
                Failure::new(
                    FailureTag::Config,
                    "host executable identity cannot be verified",
                )
            })?;
            Ok(sha256_hex(&bytes))
        })
        .clone()
}

pub(super) fn body_pins(drive: &Drive<'_>) -> Result<OwnerPins, Failure> {
    let mut identities = match drive.input.pins() {
        OwnerPins::Plan(p) => p.identities,
        _ => BTreeMap::new(),
    };
    identities.insert("input".into(), drive.input.plan_ref());
    identities.insert("body_implementation".into(), host_implementation()?);
    identities.insert(
        "body_type".into(),
        sha256_hex(drive.body.implementation_type().as_bytes()),
    );
    for (name, digest) in drive.body.dependencies().identities() {
        identities.insert(format!("body_dependency_{name}"), digest.clone());
    }
    let plan_ref = sha256_hex(
        &serde_json::to_vec(&("verified-body-owner-v1", &identities))
            .map_err(|_| Failure::new(FailureTag::Config, "body identity does not encode"))?,
    );
    Ok(PlanPins {
        plan_ref,
        identities,
    }
    .into())
}

pub(super) fn projection_pins(
    drive: &Drive<'_>,
    projection: String,
    authorities: Vec<(String, String)>,
) -> Result<OwnerPins, Failure> {
    let OwnerPins::Plan(mut pins) = body_pins(drive)? else {
        unreachable!()
    };
    pins.identities.insert("body_projection".into(), projection);
    pins.identities.insert(
        "body_authority".into(),
        sha256_hex(&serde_json::to_vec(&authorities).map_err(|_| {
            Failure::new(
                FailureTag::Config,
                "body authority identity does not encode",
            )
        })?),
    );
    pins.plan_ref = sha256_hex(
        &serde_json::to_vec(&("verified-prepared-body-owner-v1", &pins.identities)).map_err(
            |_| Failure::new(FailureTag::Config, "prepared body identity does not encode"),
        )?,
    );
    Ok(pins.into())
}
