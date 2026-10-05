//! `connector.reference`: the scheme, the template grammar and the material check.

use contextful_core::connector::reference::{check_material, Hydrated, Part, SecretName, Template, NAME_MAX};
use contextful_core::connector::ConnectorError;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Mutex;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// `secret://<name>` carries a logical name matching `[a-z0-9-]+` of at most 128 chars. A reference is never a
/// storage path, a file name or a key identifier.
// spec: connector.reference.reference-scheme@c250c1a5
#[test]
fn a_reference_is_a_lowercase_logical_name_of_at_most_128_chars() {
    assert_eq!(NAME_MAX, 128);
    for ok in ["vendor-token", "a", "k8s-2", &"x".repeat(128)] {
        assert!(SecretName::parse(ok).is_ok(), "{ok}");
    }
    for bad in ["", &"x".repeat(129), "Vendor", "vendor_token", "vault/kv/vendor", "token.json", "arn:aws:kms:key/1"] {
        assert!(SecretName::parse(bad).is_err(), "{bad}");
    }
}

/// A value embedding a credential is a template of literal text with `${secret://<name>}` placeholders, such as
/// `Bearer ${secret://vendor-token}`.
// spec: connector.reference.value-template@0c2d87f3
#[test]
fn a_template_is_literal_text_and_secret_placeholders() {
    let t = Template::parse("Bearer ${secret://vendor-token}").unwrap();
    assert_eq!(t.parts, [Part::Literal("Bearer ".into()), Part::Secret(SecretName::parse("vendor-token").unwrap())]);
    let two = Template::parse("${secret://user}:${secret://pass}").unwrap();
    assert_eq!(two.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["user", "pass"]);
    let filled = two.render(|n| Ok::<_, ()>(Hydrated::new(format!("<{n}>")))).unwrap();
    assert_eq!(filled.reveal(), "<user>:<pass>");
    assert!(!Template::parse("application/json").unwrap().has_reference());
}

/// `${env://NAME}` or a bare `${NAME}` inside a template raises `SecretForeignPlaceholder` at parse, naming the
/// value and the span.
// spec: connector.reference.foreign-placeholder@b4e26f61
#[test]
fn an_environment_or_bare_placeholder_is_foreign() {
    for (value, span) in [("Bearer ${env://VENDOR_TOKEN}", "bytes 7..28"), ("Bearer ${VENDOR_TOKEN}", "bytes 7..22")] {
        match check_material("headers.Authorization", value) {
            Err(ConnectorError::SecretForeignPlaceholder(m)) => {
                assert!(m.contains("headers.Authorization") && m.contains(span), "{m}");
                assert!(!m.contains("VENDOR_TOKEN"), "the placeholder's text is not repeated: {m}");
            }
            other => panic!("{value}: {other:?}"),
        }
    }
}

#[test]
fn no_parse_refusal_repeats_the_text_it_refused() {
    let token = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
    for value in [format!("Bearer ${{secret://{token}}}"), format!("Bearer ${{{token}"), format!("${{env://{token}}}")] {
        let e = check_material("headers.Authorization", &value).unwrap_err().to_string();
        assert!(!e.contains(token), "{e}");
    }
    let e = SecretName::parse("Sk-Live-Token").unwrap_err().to_string();
    assert!(!e.contains("Sk-Live-Token"), "{e}");
}

/// An unclosed, empty or nested placeholder raises `SecretMalformedTemplate` while the declaration is read, ahead
/// of any text leaving the process.
// spec: connector.reference.malformed-placeholder@7e945f39
#[test]
fn an_unclosed_empty_or_nested_placeholder_is_malformed() {
    for value in ["Bearer ${secret://vendor-token", "Bearer ${}", "Bearer ${ }", "${secret://${secret://inner}}"] {
        assert!(matches!(Template::parse(value), Err(ConnectorError::SecretMalformedTemplate(_))), "{value}");
    }
}

/// A credential-shaped literal standing where a reference belongs raises `SecretMaterialInDeclaration` at
/// validation, naming the key.
// spec: connector.reference.material-in-a-declaration@3ddcdc4d
#[test]
fn a_credential_literal_in_a_declaration_is_refused() {
    for value in [
        "Bearer ghp_0123456789abcdefghijABCDEFGHIJ012345", "Bearer 9f8e7d6c5b4a39218a7b", "Basic dXNlcjpodW50ZXIy", "AKIAIOSFODNN7EXAMPLE",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJlLXNpZ25hdHVyZQ",
        "sk-proj-Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4z_AbCdEfGh",
    ] {
        match check_material("headers.Authorization", value) {
            Err(ConnectorError::SecretMaterialInDeclaration(m)) => {
                assert!(m.contains("headers.Authorization"), "{m}");
                assert!(!m.contains(value), "the message repeats the material: {m}");
            }
            other => panic!("{value}: {other:?}"),
        }
    }
    for value in ["Bearer ${secret://vendor-token}", "application/json", "contextful/0.1"] {
        assert!(check_material("headers.X", value).is_ok(), "{value}");
    }
}

#[test]
fn a_hydrated_value_prints_as_a_sentinel() {
    let h = Hydrated::new("lease-9f8e7d6c5b4a3921");
    assert_eq!(format!("{h} {h:?}"), "[secret] [secret]");
    assert_eq!(h.reveal(), "lease-9f8e7d6c5b4a3921");
}

/// Frees the watched block after recording whether every byte of it read zero. The
/// integration binary's allocator; every other block passes straight to [`System`].
struct Witness;

static WATCHED: AtomicUsize = AtomicUsize::new(0);
/// 0: the watched block is still live; 1: freed zeroed; 2: freed holding a nonzero byte.
static VERDICT: AtomicU8 = AtomicU8::new(0);

unsafe impl GlobalAlloc for Witness {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if WATCHED.compare_exchange(ptr as usize, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            // The block is still owned here: `System` has not reclaimed it.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            VERDICT.store(if bytes.iter().all(|&b| b == 0) { 1 } else { 2 }, Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Witness = Witness;

/// Serialises the tests that share [`WATCHED`] and [`VERDICT`].
static WITNESS: Mutex<()> = Mutex::new(());

/// Drops `h` and reports whether its heap block read all zeros as the allocator took it back.
fn freed_zeroed(h: Hydrated) -> bool {
    let _one = WITNESS.lock().unwrap_or_else(|p| p.into_inner());
    VERDICT.store(0, Ordering::SeqCst);
    WATCHED.store(h.reveal().as_ptr() as usize, Ordering::SeqCst);
    drop(h);
    WATCHED.store(0, Ordering::SeqCst);
    match VERDICT.load(Ordering::SeqCst) {
        1 => true,
        2 => false,
        _ => panic!("the hydrated buffer was not freed when the value dropped"),
    }
}

/// The wrapper zeroes its bytes when dropped.
// spec: connector.resolve.wiped-on-drop@e8f5671c
#[test]
fn a_hydrated_value_zeroes_its_bytes_on_drop() {
    trait AmbiguousIfClone<A> {}
    impl<T: ?Sized> AmbiguousIfClone<()> for T {}
    impl<T: ?Sized + Clone> AmbiguousIfClone<u8> for T {}
    fn cannot_clone<T: AmbiguousIfClone<A>, A>() {}
    cannot_clone::<Hydrated, _>();
    // The wipe is the derived one: `Zeroize` exists only through the derive or a full
    // hand-written wipe, never through the `ZeroizeOnDrop` marker alone.
    fn wipes<T: Zeroize + ZeroizeOnDrop>(_: &T) {}
    let h = Hydrated::new("lease-9f8e7d6c5b4a3921");
    wipes(&h);
    assert!(freed_zeroed(h), "the original held its bytes after drop");
}
