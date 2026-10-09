//! `assurance.test.vacuous-exclusion` and `assurance.test.presence-before-absence`: an
//! exclusion assertion holds over an empty collection, so a test shows the collection
//! non-empty before asserting what it excludes.

use crate::{stderr, Repo};

const SUITE: &str = "crates/demo/tests/integration/double.rs";

/// A test module whose one function builds `found`, runs `checks`, then asserts `claim`.
fn module(checks: &str, claim: &str) -> String {
    format!("#[test]\nfn excludes() {{\n    let found: Vec<i32> = (1..4).map(demo::double).collect();\n{checks}    {claim}\n}}\n")
}

fn lint(r: &Repo) -> std::process::Output {
    r.run_ci(&["vacuous", SUITE])
}

// spec: assurance.test.vacuous-exclusion@ecf5142f
#[test]
fn an_exclusion_over_a_collection_never_shown_non_empty_fails_the_test_first_stage() {
    let r = Repo::init();
    let base = r.head();
    r.write(SUITE, &module("", "assert!(!found.iter().any(|x| *x == 5));"));
    r.commit("an absence claim over an unshown collection");
    let o = r.gate(&["--stage", "test-first.validate", "--base", &base]);
    assert!(!o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("VacuousAssertion"), "{err}");
    assert!(err.contains(&format!("{SUITE}:4: no presence check of `found`")), "{err}");

    let r = Repo::init();
    r.write(SUITE, &module("", "assert!(found.iter().all(|x| x % 2 == 0));"));
    let err = stderr(&lint(&r));
    assert!(err.contains(&format!("VacuousAssertion: {SUITE}:4")), "{err}");
}

#[test]
fn only_the_lines_a_change_adds_are_held() {
    let r = Repo::init();
    r.write(SUITE, &module("", "assert!(!found.iter().any(|x| *x == 5));"));
    let base = r.commit("an older absence claim");
    r.write("crates/demo/tests/integration/double.rs", &format!("{}\n#[test]\nfn doubles_two() {{\n    assert_eq!(demo::double(2), 4);\n}}\n", std::fs::read_to_string(r.root.join(SUITE)).unwrap()));
    r.commit("a new test beside it");
    let o = r.gate(&["--stage", "test-first.validate", "--base", &base]);
    assert!(!lint(&r).status.success(), "the whole file holds the older claim");
    assert!(!stderr(&o).contains("VacuousAssertion"), "{}", stderr(&o));
}

// spec: assurance.test.presence-before-absence@50712221
#[test]
fn a_presence_check_before_the_exclusion_admits_it() {
    let claim = "assert!(!found.iter().any(|x| *x == 5));";
    for checks in [
        "    assert!(!found.is_empty());\n",
        "    assert_eq!(found.len(), 3);\n",
        "    assert_eq!(found, [2, 4, 6]);\n",
        "    assert!(found.iter().any(|x| *x == 2));\n",
        "    let first = found[0];\n    assert_eq!(first, 2);\n",
    ] {
        let r = Repo::init();
        r.write(SUITE, &module(checks, claim));
        let o = lint(&r);
        assert!(o.status.success(), "{checks}: {}", stderr(&o));
    }
    let r = Repo::init();
    r.write(SUITE, &module("", "assert!(!found.is_empty() && found.iter().all(|x| x % 2 == 0));"));
    assert!(lint(&r).status.success(), "{}", stderr(&lint(&r)));
}

// spec: assurance.test.guard-fires-both-ways@aa3590a0
#[test]
fn the_vacuous_guard_refuses_its_motivating_fixture_and_admits_the_state_it_guards() {
    // The motivating fixture: an absence claim whose collection a broken producer leaves
    // empty, so the claim holds whatever the producer does.
    let claim = "assert!(!found.iter().any(|x| *x == 5));";
    for checks in ["", "    let _ = found.is_empty();\n", "    assert!(found.iter().all(|x| *x > 0));\n"] {
        let r = Repo::init();
        r.write(SUITE, &module(checks, claim));
        let o = lint(&r);
        assert!(!o.status.success(), "the guard admitted {checks:?}");
        assert!(stderr(&o).contains("VacuousAssertion"), "{}", stderr(&o));
    }
    let r = Repo::init();
    r.write(SUITE, &module("    assert!(!found.is_empty());\n", claim));
    assert!(lint(&r).status.success(), "{}", stderr(&lint(&r)));
}

#[test]
fn a_constant_or_a_literal_collection_is_present_by_construction() {
    let r = Repo::init();
    r.write(
        SUITE,
        "const KINDS: [&str; 2] = [\"a\", \"b\"];\n#[test]\nfn fixed() {\n    assert!(KINDS.iter().all(|k| k.len() == 1));\n    assert!([1, 2].iter().all(|x| *x > 0));\n    assert!(!vec![1, 2].iter().any(|x| *x == 0));\n}\n",
    );
    assert!(lint(&r).status.success(), "{}", stderr(&lint(&r)));
}
