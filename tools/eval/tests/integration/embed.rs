use contextful_eval::embed::*;

use crate::EPS;

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum()
}

#[test]
fn the_stub_embedder_is_seeded_unit_length_and_word_bounded() {
    let e = StubEmbedder::new(DEFAULT_SEED);
    let v = e.embed("Modern art museums");
    assert_eq!(v.len(), STUB_DIM);
    assert!((dot(&v, &v) - 1.0).abs() < 1e-6, "a unit vector");
    assert_eq!(v, e.embed("modern ART museum"), "case and a trailing plural fold into one feature");
    assert_ne!(v, StubEmbedder::new(DEFAULT_SEED + 1).embed("modern art museums"), "the seed moves every feature");
    assert!(!e.embed("a ! ?").is_empty(), "the exclusion below ranges over no element");
    assert!(e.embed("a ! ?").iter().all(|x| *x == 0.0), "no feature embeds as zero");

    // A word never shares a feature with a longer word containing it.
    assert_eq!(features("art article party"), ["art", "article", "party"]);
    assert!((dot(&e.embed("art"), &e.embed("art gallery")) - 1.0 / 2f64.sqrt()).abs() < 1e-6);
}

#[test]
fn a_non_ascii_run_embeds_as_character_bigrams() {
    assert_eq!(features("東京駅で会う"), ["東京", "京駅", "駅で", "で会", "会う"]);
    assert_eq!(features("駅"), ["駅"]);
    let e = StubEmbedder::new(DEFAULT_SEED);
    assert!(dot(&e.embed("東京駅"), &e.embed("東京駅で会う")) > 0.5 - EPS, "a phrase inside a sentence shares its bigrams");
}
