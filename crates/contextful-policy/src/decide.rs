//! The decision module (`assurance.structure-tree.decision-module`): one case text in, one
//! decision out. A `verify` case is a network checkpoint's admission of one credential;
//! every other case is one of `contextful_core::decide`'s domain decisions.
//!
//! The same source builds native and, for `wasm32-unknown-unknown`, as a module exporting
//! [`abi`]'s three functions, which a gateway and the differential harness call. The
//! WebAssembly build imports one function: the credential library's evaluator clock,
//! `performance_now` under the `__wbindgen_placeholder__` module, milliseconds as `f64`.
//! Verification draws no randomness; a randomness request on that target fails.

use crate::keyset::{KeySource, StaticPins};
use crate::revoke::{parse_denylist, RevocationState};
use crate::verify::{no_holder_proof, verify_network, Admission};
use contextful_core::decide::{decide_value, field, required, string, strings, unsigned, Decision, Decoded, Fault, CASE_MALFORMED};
use contextful_core::grant::Action;
use contextful_core::time::Instant;
use contextful_core::AuthorityError;
use serde_json::{Map, Value};

/// The operation a credential case names.
pub const VERIFY: &str = "verify";

/// The decision on one case text: text that is not UTF-8 or not JSON is malformed.
pub fn decide(case: &[u8]) -> Decision {
    match std::str::from_utf8(case).ok().and_then(|text| serde_json::from_str::<Value>(text).ok()) {
        Some(value) => decide_case(&value),
        None => Decision::refused(CASE_MALFORMED, None),
    }
}

/// The decision on one decoded case.
pub fn decide_case(case: &Value) -> Decision {
    match case.get("op").and_then(Value::as_str) {
        Some(VERIFY) => case.as_object().ok_or(Fault::Malformed).and_then(verify).unwrap_or_else(Fault::decision),
        _ => decide_value(case),
    }
}

/// `{"op":"verify","credential":…,"keys":[pin…],"at":<unix s>,"audience"?,"denylist"?:[id…],
/// "action"?,"tables"?:[name…]}`: admission at a network checkpoint with no holder proof,
/// then, where the case names an action, whether one admitted grant carries it over every
/// named table.
fn verify(obj: &Map<String, Value>) -> Decoded<Decision> {
    let credential = string(required(obj, "credential")?)?;
    let pins = strings(required(obj, "keys")?)?;
    let at = unsigned(required(obj, "at")?)?;
    let audience = field(obj, "audience").map(string).transpose()?;
    let denylist = field(obj, "denylist").map(strings).transpose()?.unwrap_or_default();
    let action = field(obj, "action").map(string).transpose()?;
    let tables = field(obj, "tables").map(strings).transpose()?;
    let request = match (action, tables) {
        (Some(action), Some(tables)) => Some((Action::parse(action)?, tables)),
        (None, None) => None,
        _ => return Err(Fault::Malformed),
    };
    let at = Instant::from_unix_secs(i64::try_from(at).map_err(|_| Fault::Malformed)?)?;
    let keys = StaticPins::parse(&pins.join(","))?.keys()?;
    let revocation = RevocationState { denylist: parse_denylist(&denylist.join("\n"), ""), ..RevocationState::default() };
    let admission = Admission::new(at, &revocation);
    let admission = match audience {
        Some(aud) => admission.expecting(aud),
        None => admission,
    };
    let admitted = verify_network(credential, &keys, &admission, no_holder_proof::<AuthorityError>)?;
    Ok(match request {
        None => Decision::verdict("admitted"),
        Some((action, tables)) => {
            let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
            Decision::verdict(if admitted.permits(action, &tables) { "admitted" } else { "not_covered" })
        }
    })
}

/// The module's exports on `wasm32-unknown-unknown`. The host copies a case into memory
/// from `contextful_alloc(len)`, calls `contextful_decide(ptr, len)`, which returns the
/// decision JSON's address in the high 32 bits and its length in the low 32, reads it, and
/// hands both regions back through `contextful_free(ptr, len)`.
#[cfg(target_arch = "wasm32")]
pub mod abi {
    /// Verification draws no randomness; a request for it on this target fails.
    fn no_randomness(_: &mut [u8]) -> Result<(), getrandom::Error> {
        Err(getrandom::Error::UNSUPPORTED)
    }
    getrandom::register_custom_getrandom!(no_randomness);

    /// A region of `len` bytes the host fills with a case.
    #[no_mangle]
    pub extern "C" fn contextful_alloc(len: u32) -> u32 {
        let region = vec![0u8; len as usize].into_boxed_slice();
        Box::into_raw(region) as *mut u8 as u32
    }

    /// Release a region this module handed out, input or output.
    ///
    /// # Safety
    /// `ptr` and `len` are one region `contextful_alloc` or `contextful_decide` returned.
    #[no_mangle]
    pub unsafe extern "C" fn contextful_free(ptr: u32, len: u32) {
        let region = std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(Box::from_raw(region));
    }

    /// Decide the case at `ptr..ptr+len`.
    ///
    /// # Safety
    /// `ptr` and `len` are a region `contextful_alloc` returned, filled by the host.
    #[no_mangle]
    pub unsafe extern "C" fn contextful_decide(ptr: u32, len: u32) -> u64 {
        let case = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        let out = serde_json::to_vec(&super::decide(case)).unwrap_or_default().into_boxed_slice();
        let len = out.len() as u64;
        let at = Box::into_raw(out) as *mut u8 as u64;
        (at << 32) | len
    }
}
