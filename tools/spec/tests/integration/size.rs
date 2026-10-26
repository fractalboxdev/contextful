//! Size ratchets: the word ceilings of `corpus.anatomy.word-ceilings`, the advisory of
//! `corpus.anatomy.long-operation`, and the unowned clauses of `corpus.state.unowned`.

use crate::{codes, Scratch};

const STORE: &str = "spec/10-store.md";

fn ceilings(s: &Scratch) -> Vec<String> {
    let mut found = codes(&s.lint("anatomy"), "SpecWordCeiling");
    found.extend(codes(&s.lint("rationale"), "SpecWordCeiling"));
    found
}

/// Append ` word` to the first line of `rel` starting with `prefix`.
fn grow(s: &Scratch, rel: &str, prefix: &str) {
    let text = s.read(rel);
    let line = text.lines().find(|l| l.starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` line in {rel}")).to_string();
    s.write(rel, &text.replacen(&line, &format!("{line} word"), 1));
}

fn ceiling_of(s: &Scratch, key: &str) -> usize {
    let (table, name) = key.split_once('.').unwrap();
    let text = s.read("spec/terms/corpus.toml");
    let mut inside = false;
    for l in text.lines() {
        if l.starts_with('[') {
            inside = l.trim() == format!("[ceiling.{table}]");
        } else if inside {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim().trim_matches('"') == name {
                    return v.trim().parse().unwrap();
                }
            }
        }
    }
    panic!("no ceiling `{key}`")
}

// spec: corpus.anatomy.word-ceilings@f1fb0e20
#[test]
fn a_contract_or_an_adr_one_word_over_its_ceiling_fails_lint() {
    let s = Scratch::copy();
    assert!(ceilings(&s).is_empty(), "{:?}", ceilings(&s));
    let store = ceiling_of(&s, "contract-words.store");
    grow(&s, STORE, "- `includes-runs` — ");
    assert_eq!(ceilings(&s), [format!("ratchet `contract-words.store` counts {}, over its ceiling {store}", store + 1)]);
    let out = s.cmd(&["ceilings"]);
    assert!(!out.status.success(), "a ceiling never rises");

    let s = Scratch::copy();
    let adr = ceiling_of(&s, "adr-words.A-store");
    grow(&s, "spec/adr/A-store.md", "**Status:**");
    assert_eq!(ceilings(&s), [format!("ratchet `adr-words.A-store` counts {}, over its ceiling {adr}", adr + 1)]);

    let s = Scratch::copy();
    let text = s.read(STORE);
    let line = text.lines().find(|l| l.starts_with("- `includes-runs` — ")).unwrap().to_string();
    let shorter = line.rsplit_once(' ').unwrap().0.to_string() + ".";
    s.write(STORE, &text.replacen(&line, &shorter, 1));
    assert_eq!(ceilings(&s), [format!("ratchet `contract-words.store` counts {}, under its ceiling {store}; run `contextful-spec ceilings`", store - 1)]);
    assert!(s.cmd(&["ceilings"]).status.success());
    assert_eq!(ceiling_of(&s, "contract-words.store"), store - 1);
    assert!(ceilings(&s).is_empty(), "{:?}", ceilings(&s));
}

// spec: corpus.anatomy.long-operation@c015dab7
#[test]
fn state_prints_an_advisory_for_each_operation_over_its_clause_bound() {
    let s = Scratch::copy();
    let out = s.cmd(&["state"]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.lines().any(|l| l.starts_with("advisory: `connector.source` holds ") && l.ends_with(" clauses, over 30")), "{err}");
    assert!(!err.contains("`store.fold`"), "{err}");
}

fn unowned(s: &Scratch) -> Vec<String> {
    codes(&s.lint("state"), "SpecClauseUnowned")
}

/// Set the `clause-unowned` ceiling in the scratch fragment.
fn set_unowned(s: &Scratch, value: usize) {
    let text = s.read("spec/terms/corpus.toml");
    let old = text.lines().find(|l| l.starts_with("clause-unowned ")).unwrap().to_string();
    s.write("spec/terms/corpus.toml", &text.replacen(&old, &format!("clause-unowned = {value}"), 1));
}

// spec: corpus.state.unowned@a260298a
#[test]
fn an_unpinned_unscheduled_unquestioned_clause_counts_as_unowned() {
    let s = Scratch::copy();
    set_unowned(&s, 0);
    let found = unowned(&s);
    let subscribe: Vec<&String> = found.iter().filter(|m| m.contains("`read.subscribe.")).collect();
    assert!(!subscribe.is_empty(), "read.subscribe is in no milestone: {found:?}");
    assert_eq!(found.iter().filter(|m| m.contains("`read.traverse.")).count(), 0, "an open milestone names read.traverse");

    let first = subscribe[0].split('`').nth(1).unwrap().to_string();
    let subject = first.rsplit('.').next().unwrap().to_string();
    let text = s.read("spec/20-read.md");
    let at = text.find("\n## subscribe\n").unwrap();
    let end = at + 1 + text[at + 1..].find("\n## ").unwrap();
    let line = format!("\nunsettled: Does `{subject}` hold under replay? owner: read affects: read.subscribe\n");
    s.write("spec/20-read.md", &format!("{}{line}{}", &text[..end], &text[end..]));
    let after = unowned(&s);
    assert_eq!(after.len(), found.len() - 1, "{after:?}");
    assert_eq!(after.iter().filter(|m| m.contains(&format!("`{first}`"))).count(), 0, "{after:?}");

    set_unowned(&s, after.len() + 1);
    assert_eq!(unowned(&s), [format!("ratchet `clause-unowned` counts {}, under its ceiling {}; run `contextful-spec ceilings`", after.len(), after.len() + 1)]);
}
