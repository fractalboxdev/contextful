//! `authority.mask`: the class registry, column declarations and their checks, and the
//! pepper-keyed digest and token.

use contextful_core::enforce::EnforceError;
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::{Column, ColumnType, FloatItem};
use contextful_policy::enforce::mask::{
    class, truncation_ceiling, Pepper, CLASSES, HASH_OUTPUT_WIDTH, MASKS_PER_TABLE, MASK_CROWD_FLOOR, PEPPER_VAR,
    TOKEN_OUTPUT_WIDTH,
};
use contextful_policy::enforce::policy::TablePolicy;
use contextful_policy::enforce::PolicyError;

fn policy(columns: &str) -> Result<TablePolicy, PolicyError> {
    let toml = format!("[[pipeline.tables]]\nname = \"patients\"\n[pipeline.tables.policy.columns]\n{columns}\n");
    TablePolicy::from_decl(&TableDecl::parse_pipeline(&toml).unwrap().remove(0))
}

fn refused(columns: &str) -> EnforceError {
    match policy(columns) {
        Err(PolicyError::Enforce(e)) => e,
        other => panic!("{columns}: {other:?}"),
    }
}

fn pepper(key: &str) -> Pepper {
    let key = key.to_string();
    Pepper::resolve(move |v| (v == PEPPER_VAR).then(|| key.clone()))
}

/// A column declares `class` and `strategy`, optionally `combine`, `crowd` and `summarize_only`, under `[pipeline.tables.policy.columns]`. An undeclared column carries no class.
// spec: authority.mask.column-policy@bb522f07
#[test]
fn a_column_declares_its_class_and_strategy() {
    let p = policy(
        "patient_mrn = { class = \"mrn\", strategy = \"hash\", combine = \"truncate:4\", crowd = 1000 }\n\
         case_notes = { class = \"phi\", summarize_only = true }\ninternal_id = { strategy = \"hash\" }",
    )
    .unwrap();
    assert_eq!(p.columns["patient_mrn"].class.unwrap().name, "mrn");
    assert!(p.columns["patient_mrn"].mask.is_some());
    assert!(p.columns["case_notes"].summarize_only && p.columns["case_notes"].mask.is_none());
    assert!(p.columns["internal_id"].class.is_none());
    assert!(!p.columns.contains_key("visit_date"), "an undeclared column carries no class");
    assert!(matches!(policy("x = { strategy = \"hash\", salt = \"s\" }"), Err(PolicyError::Malformed(_))));
}

/// `class` takes a value from the class registry in Shapes. `phi` marks protected health data; `ssn`, `phone`, `email` and `mrn` are exhaustible, each carrying a registered domain size.
// spec: authority.mask.class-registry@932c61c2
#[test]
fn the_class_registry_holds_five_classes() {
    let names: Vec<(&str, Option<f64>)> = CLASSES.iter().map(|c| (c.name, c.domain)).collect();
    assert_eq!(names, [("phi", None), ("ssn", Some(1e9)), ("phone", Some(1e10)), ("email", Some(1e10)), ("mrn", Some(1e8))]);
    assert!(class("phi").unwrap().protected() && !class("phi").unwrap().exhaustible());
    assert!(["ssn", "phone", "email", "mrn"].iter().all(|c| class(c).unwrap().exhaustible()));
}

/// A `class` outside the registry raises `EnforceUnknownClass` at manifest check.
// spec: authority.mask.unknown-class@d07fb887
#[test]
fn an_unknown_class_is_refused() {
    assert!(matches!(refused("x = { class = \"emial\", strategy = \"hash\" }"), EnforceError::UnknownClass(w) if w.contains("emial")));
}

/// `hash` or `tokenize` standing alone over an exhaustible class raises `EnforceDigestAloneOnExhaustibleClass`.
// spec: authority.mask.digest-alone@efcea9bc
#[test]
fn a_bare_digest_over_an_exhaustible_class_is_refused() {
    for strategy in ["hash", "tokenize"] {
        for c in ["ssn", "phone", "email", "mrn"] {
            let e = refused(&format!("x = {{ class = \"{c}\", strategy = \"{strategy}\" }}"));
            assert!(matches!(e, EnforceError::DigestAloneOnExhaustibleClass(_)), "{strategy} {c}: {e:?}");
        }
    }
    assert!(policy("x = { class = \"email\", strategy = \"tokenize\", combine = \"truncate:4\" }").is_ok());
    assert!(policy("x = { class = \"phi\", strategy = \"tokenize\" }").is_ok(), "phi is not exhaustible");
}

/// A column of random identifiers or opaque tokens carries `hash` with no combine.
// spec: authority.mask.high-entropy@14f5dbbf
#[test]
fn a_classless_identifier_takes_a_bare_hash() {
    assert!(policy("session_token = { strategy = \"hash\" }").is_ok());
}

/// A `bucket` or `range` combine behind `hash` or `tokenize` raises `EnforceCombineWithoutGeneralization`; `truncate` is the one combine that generalizes a digest.
// spec: authority.mask.combine-generalizes@c088c47d
#[test]
fn only_truncate_combines_behind_a_digest() {
    for (primary, combine) in [("hash", "bucket:10"), ("hash", "range:10"), ("tokenize", "bucket:10"), ("tokenize", "hash")] {
        let e = refused(&format!("x = {{ strategy = \"{primary}\", combine = \"{combine}\" }}"));
        assert!(matches!(e, EnforceError::CombineWithoutGeneralization(_)), "{primary}+{combine}: {e:?}");
    }
    assert!(policy("x = { strategy = \"tokenize\", combine = \"truncate:8\" }").is_ok());
}

/// `hash` emits 32 chars of hexadecimal.
// spec: authority.mask.hash-width@c7e0463b
#[test]
fn a_digest_is_32_hex_chars() {
    assert_eq!(HASH_OUTPUT_WIDTH, 32);
    let d = pepper("k").digest("dana@acme.example");
    assert_eq!(d.len(), 32);
    assert!(d.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{d}");
}

/// `tokenize` emits 20 chars.
// spec: authority.mask.token-width@18a2b8f7
#[test]
fn a_token_is_20_chars() {
    assert_eq!(TOKEN_OUTPUT_WIDTH, 20);
    assert_eq!(pepper("k").token("dana@acme.example").chars().count(), 20);
    assert_eq!(pepper("k").token("").chars().count(), 20);
}

/// A `truncate` combine at or past its primary's output width raises `EnforceTruncationCutsNothing`.
// spec: authority.mask.cuts-nothing@d99de0f2
#[test]
fn a_truncation_at_the_output_width_is_refused() {
    for (primary, width) in [("hash", 32), ("tokenize", 20)] {
        let e = refused(&format!("x = {{ strategy = \"{primary}\", combine = \"truncate:{width}\" }}"));
        assert!(matches!(e, EnforceError::TruncationCutsNothing(_)), "{e:?}");
        assert!(policy(&format!("x = {{ strategy = \"{primary}\", combine = \"truncate:{}\" }}", width - 1)).is_ok());
    }
}

/// A column's `crowd`, the fewest inputs one masked output covers, is at least 1000 values and defaults to that floor.
// spec: authority.mask.crowd@8f07d843
#[test]
fn the_crowd_is_at_least_1000_and_defaults_to_it() {
    assert_eq!(MASK_CROWD_FLOOR, 1000);
    assert_eq!(policy("x = { strategy = \"hash\" }").unwrap().columns["x"].crowd, 1000);
    assert_eq!(policy("x = { strategy = \"hash\", crowd = 5000 }").unwrap().columns["x"].crowd, 5000);
    assert!(matches!(policy("x = { strategy = \"hash\", crowd = 999 }"), Err(PolicyError::Malformed(_))));
}

/// Behind a keyed digest over an exhaustible class, a `truncate` width above floor(log_b(domain ÷ `crowd`)), b being 16 for `hash` and 36 for `tokenize`, with domain from the class registry, raises `EnforceTruncationTooWide`.
// spec: authority.mask.truncation-ceiling@612f331a
#[test]
fn a_truncation_past_the_class_ceiling_is_refused() {
    assert_eq!(
        [truncation_ceiling(1e9, 1000, 16), truncation_ceiling(1e10, 1000, 16), truncation_ceiling(1e8, 1000, 16)],
        [4, 5, 4]
    );
    assert_eq!(truncation_ceiling(1e10, 1_000_000, 16), 3);
    assert_eq!([truncation_ceiling(1e10, 1000, 36), truncation_ceiling(1e9, 1000, 36)], [4, 3]);
    assert!(policy("x = { class = \"email\", strategy = \"tokenize\", combine = \"truncate:4\" }").is_ok());
    let e = refused("x = { class = \"email\", strategy = \"tokenize\", combine = \"truncate:5\" }");
    assert!(matches!(e, EnforceError::TruncationTooWide(_)), "a 5-char token covers 36^5 values: {e:?}");
    assert!(policy("x = { class = \"email\", strategy = \"hash\", combine = \"truncate:5\" }").is_ok());
    let e = refused("x = { class = \"email\", strategy = \"hash\", combine = \"truncate:6\" }");
    assert!(matches!(e, EnforceError::TruncationTooWide(_)), "{e:?}");
    let e = refused("x = { class = \"email\", strategy = \"hash\", combine = \"truncate:5\", crowd = 1000000 }");
    assert!(matches!(e, EnforceError::TruncationTooWide(_)), "{e:?}");
}

/// `hash` output is an HMAC-SHA-256 under the pepper and is pseudonymous; no surface labels a masked value anonymous.
// spec: authority.mask.pseudonymous@3327c8c9
#[test]
fn the_digest_is_an_hmac_under_the_pepper() {
    // HMAC-SHA-256(key = "key", message = "hash\0value"), first 16 bytes.
    let expected = {
        use sha2::{Digest, Sha256};
        let mut k = [0u8; 64];
        k[..3].copy_from_slice(b"key");
        let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        let inner = Sha256::new().chain_update(&ipad).chain_update(b"hash\0value").finalize();
        let outer = Sha256::new().chain_update(&opad).chain_update(inner).finalize();
        outer[..16].iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    assert_eq!(pepper("key").digest("value"), expected);
    assert_ne!(pepper("other").digest("value"), expected, "a different pepper, a different digest");
    assert_eq!(pepper("key").digest("value"), pepper("key").digest("value"), "the same input joins on the same digest");
}

/// Left unset, the pepper resolves to a constant in the source tree and the process emits one signal per run.
// spec: authority.mask.development-pepper@51282a22
#[test]
fn an_unset_pepper_resolves_to_the_development_constant() {
    let unset = Pepper::resolve(|_| None);
    let empty = Pepper::resolve(|_| Some(String::new()));
    assert!(unset.is_development() && empty.is_development());
    assert_eq!(unset.digest("v"), empty.digest("v"));
    assert!(unset.signal().unwrap().contains(PEPPER_VAR));
    assert!(pepper("k").signal().is_none());
    assert!(!format!("{unset:?}").contains("development-pepper"), "the key never prints");
}

/// A table carries at most 128 entries in its column mask set.
// spec: authority.mask.masks-per-table@a689b478
#[test]
fn a_table_carries_at_most_128_masks() {
    assert_eq!(MASKS_PER_TABLE, 128);
    let masks = |n: usize| (0..n).map(|i| format!("c{i} = {{ strategy = \"drop\" }}")).collect::<Vec<_>>().join("\n");
    assert!(policy(&masks(128)).is_ok());
    assert!(matches!(policy(&masks(129)), Err(PolicyError::Malformed(_))));
}

/// A mask naming a column the table's schema omits raises `EnforceMaskOnAbsentColumn` at manifest load, refusing the whole manifest.
// spec: authority.mask.absent-column@04c44ecc
#[test]
fn a_mask_on_an_absent_column_is_refused() {
    let p = policy("contact_email = { class = \"email\", strategy = \"tokenize\", combine = \"truncate:4\" }").unwrap();
    let schema = [Column::new("patient_id", ColumnType::Utf8, true)];
    match p.check_schema("patients", &schema) {
        Err(EnforceError::MaskOnAbsentColumn(why)) => assert!(why.contains("contact_email"), "{why}"),
        other => panic!("{other:?}"),
    }
    let schema = [Column::new("contact_email", ColumnType::Utf8, true)];
    assert_eq!(p.check_schema("patients", &schema), Ok(()));
}

/// A binary column masks by `drop` or by `hash` over its bytes, a vector column by `drop` alone; another strategy on either raises `EnforceStrategyOutsideType` at manifest load.
// spec: authority.mask.typed-strategy@e5c57628
#[test]
fn binary_and_vector_columns_admit_their_strategies_alone() {
    let binary = [ColumnType::Binary, ColumnType::FixedSizeBinary(32)];
    let vector = [ColumnType::FixedSizeList(FloatItem::Float32, 4), ColumnType::FixedSizeList(FloatItem::Float16, 384)];
    let strategies = [
        ("drop", true, true),
        ("hash", true, false),
        ("tokenize", false, false),
        ("truncate:4", false, false),
        ("bucket:5", false, false),
        ("range:5", false, false),
    ];
    for (strategy, on_binary, on_vector) in strategies {
        let p = policy(&format!("v = {{ strategy = \"{strategy}\" }}")).unwrap();
        for (types, admitted) in [(&binary, on_binary), (&vector, on_vector)] {
            for ty in types {
                match (p.check_schema("patients", &[Column::new("v", *ty, true)]), admitted) {
                    (Ok(()), true) => {}
                    (Err(EnforceError::StrategyOutsideType(why)), false) => {
                        assert!(why.contains("`v`") && why.contains(&ty.name()), "{why}");
                        assert_eq!(EnforceError::StrategyOutsideType(why).identifier(), "EnforceStrategyOutsideType");
                    }
                    (other, _) => panic!("{strategy} on {ty:?}: {other:?}"),
                }
            }
        }
        // A scalar column admits every strategy.
        assert_eq!(p.check_schema("patients", &[Column::new("v", ColumnType::Utf8, true)]), Ok(()));
    }
    // A combine behind `hash` truncates the digest of the bytes.
    let p = policy("v = { strategy = \"hash\", combine = \"truncate:6\" }").unwrap();
    assert_eq!(p.check_schema("patients", &[Column::new("v", ColumnType::Binary, true)]), Ok(()));
}

/// `hash` over a binary column digests the bytes the padded base64 carries, so it joins a text digest of the same bytes; `drop` nulls a binary or vector cell.
#[test]
fn a_binary_hash_digests_the_bytes() {
    let key = pepper("key");
    let p = policy("v = { strategy = \"hash\" }").unwrap();
    let hash = p.columns["v"].mask.as_ref().unwrap();
    // "dmFsdWU=" is the base64 of the bytes of "value".
    assert_eq!(hash.apply(&key, Some("dmFsdWU="), ColumnType::Binary), Some(key.digest("value")));
    assert_eq!(hash.apply(&key, Some("dmFsdWU="), ColumnType::FixedSizeBinary(5)), Some(key.digest_bytes(b"value")));
    assert_eq!(key.digest_bytes(&[0xFF, 0x00]).len(), HASH_OUTPUT_WIDTH as usize);
    assert_eq!(hash.sql("v", ColumnType::Binary), "contextful_mask_hash_bytes(\"v\")");
    let p = policy("v = { strategy = \"drop\" }").unwrap();
    let drop = p.columns["v"].mask.as_ref().unwrap();
    for ty in [ColumnType::Binary, ColumnType::FixedSizeList(FloatItem::Float16, 4)] {
        assert_eq!(drop.apply(&key, Some("dmFsdWU="), ty), None);
    }
    assert_eq!(drop.sql("v", ColumnType::FixedSizeList(FloatItem::Float16, 4)), "CAST(NULL AS FLOAT[4])");
    assert_eq!(drop.sql("v", ColumnType::FixedSizeBinary(5)), "CAST(NULL AS BLOB)");
}
