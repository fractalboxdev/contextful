//! `corpus.topic`: a topic page and its registry entry, the clauses it answers within one
//! pointer step, and the bounds on its words, its pointers into one contract and the
//! number of pages.

use crate::{codes, Scratch};

const PAGE: &str = "spec/topics/concurrent-writers.md";
const REGISTRY: &str = "spec/terms/topic.toml";

fn topic(s: &Scratch) -> Vec<String> {
    codes(&s.lint("topic"), "SpecTopic")
}

/// Append `text` as a paragraph of the concurrent-writers page.
fn append(s: &Scratch, text: &str) {
    let page = s.read(PAGE);
    s.write(PAGE, &format!("{page}\n{text}\n"));
}

/// A page `slug` and its entry, holding one pointer.
fn add_topic(s: &Scratch, slug: &str) {
    let registry = s.read(REGISTRY);
    s.write(REGISTRY, &format!("{registry}\n[topic.{slug}]\ntitle = \"Topic {slug}\"\nanswers = []\n"));
    s.write(&format!("spec/topics/{slug}.md"), &format!("---\ntopic: {slug}\n---\n\n# Topic {slug}\n\nA page ({{{{store.lease.held}}}}).\n"));
}

// spec: corpus.topic.file@1241a31e
#[test]
fn a_topic_page_carries_its_registry_title_and_at_most_600_words() {
    let s = Scratch::copy();
    assert!(topic(&s).is_empty(), "{:?}", topic(&s));

    append(&s, &vec!["word"; 600].join(" "));
    let found = topic(&s);
    assert!(found.iter().any(|m| m.contains("words exceed 600")), "{found:?}");

    let s = Scratch::copy();
    s.write(PAGE, &s.read(PAGE).replacen("# Concurrent writers", "# Racing writers", 1).replacen("topic: concurrent-writers", "topic: racing", 1));
    let found = topic(&s);
    assert!(found.iter().any(|m| m.contains("title `Racing writers` differs from registry `Concurrent writers`")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("front matter does not say `topic: concurrent-writers`")), "{found:?}");
}

// spec: corpus.topic.pages@9361a0e0
#[test]
fn the_corpus_holds_at_most_8_topic_pages() {
    let s = Scratch::copy();
    for i in 1..=7 {
        add_topic(&s, &format!("extra-{i}"));
    }
    assert!(topic(&s).is_empty(), "{:?}", topic(&s));
    add_topic(&s, "extra-8");
    let found = topic(&s);
    assert!(found.iter().any(|m| m.contains("9 topic pages exceed 8")), "{found:?}");
}

// spec: corpus.topic.answers@5fc0aab1
#[test]
fn each_answer_sits_within_one_pointer_step_of_its_page() {
    let s = Scratch::copy();
    let registry = s.read(REGISTRY);
    assert!(registry.contains("\"store.lease.stale-fence\""), "an answer reached through a pointed clause's statement");
    s.write(REGISTRY, &registry.replacen("answers = [\n", "answers = [\n  \"store.fold.retention\",\n  \"store.fold.no-such-clause\",\n", 1));
    let found = topic(&s);
    assert!(found.iter().any(|m| m.contains("answer `store.fold.retention` is not within one pointer step of the page")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("topic `concurrent-writers` answers missing clause `store.fold.no-such-clause`")), "{found:?}");
}

// spec: corpus.topic.contract-pointers@4f1c8786
#[test]
fn a_topic_page_holds_at_most_3_pointers_into_one_contract() {
    let s = Scratch::copy();
    append(&s, "Again ({{store.lease.held}}).");
    assert!(topic(&s).is_empty(), "{:?}", topic(&s));
    append(&s, "Retention ({{store.fold.retention}}).");
    let found = topic(&s);
    assert!(found.iter().any(|m| m.contains("4 pointers into `store` exceed 3")), "{found:?}");
}

// spec: corpus.topic.non-normative@5043e8ad
#[test]
fn a_topic_page_restates_no_rule_and_no_contract_file_links_one() {
    let s = Scratch::copy();
    append(&s, "- `restated` — A run finding a held lease raises `LeaseHeld`.");
    s.write("spec/10-store.md", &format!("{}\nSee [the topic](topics/concurrent-writers.md).\n", s.read("spec/10-store.md")));
    let found = topic(&s);
    assert!(found.iter().any(|m| m.contains("clause item in a topic page")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("topic page names error `LeaseHeld`")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("contract file links a topic page")), "{found:?}");
}

// spec: corpus.topic.bad-topic@0709cba0
#[test]
fn a_page_without_an_entry_or_an_entry_without_a_page_raises_spec_topic() {
    let s = Scratch::copy();
    s.write("spec/topics/orphan.md", "---\ntopic: orphan\n---\n\n# Orphan\n\nNo entry.\n");
    let registry = s.read(REGISTRY);
    s.write(REGISTRY, &format!("{registry}\n[topic.missing]\ntitle = \"Missing\"\n"));
    let found = codes(&s.lint("topic"), "SpecTopic");
    assert!(found.iter().any(|m| m.contains("topic page `orphan` has no entry")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("topic `missing` has no page spec/topics/missing.md")), "{found:?}");
}
