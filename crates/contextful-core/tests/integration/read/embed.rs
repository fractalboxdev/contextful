//! `read.embed`: the default embedder and the caller-supplied query embedding.

use contextful_core::read::embed::{cosine, Embedder, HashingEmbedder, HASHING_DIMENSIONS};
use contextful_core::read::rank::{fuse, min_max};

/// The embedding capability is a port whose default is deterministic and I/O-free: it hashes token term frequencies into an L2-normalized vector, needing no download, key or network call.
// spec: read.embed.default-embedder@35a3acd6
#[test]
fn the_default_embedder_is_deterministic_and_normalized() {
    let e: &dyn Embedder = &HashingEmbedder::default();
    assert_eq!(e.dimensions(), HASHING_DIMENSIONS);
    let a = e.embed("Solar battery storage, battery prices");
    assert_eq!(a, HashingEmbedder::default().embed("Solar battery storage, battery prices"));
    let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "{norm}");
    // Term frequency counts: the repeated token outweighs a single one.
    let heavy = e.embed("battery battery battery solar");
    let battery = e.embed("battery");
    let solar = e.embed("solar");
    assert!(cosine(&heavy, &battery).unwrap() > cosine(&heavy, &solar).unwrap());
    assert!(!e.embed("").is_empty(), "the exclusion below ranges over no element");
    assert!(e.embed("").iter().all(|x| *x == 0.0));
}

/// The default is a lexical-vector baseline: a paraphrase is orthogonal under it and cross-lingual recall is undefined. Semantic reach comes from the learned in-process model or a caller-supplied query embedding.
// spec: read.embed.default-embedder-reach@6402e25b
#[test]
fn a_paraphrase_is_orthogonal_under_the_default() {
    let e = HashingEmbedder::default();
    let sim = cosine(&e.embed("automobile"), &e.embed("car")).unwrap();
    assert!(sim.abs() < 1e-9, "{sim}");
    assert!(cosine(&e.embed("car"), &e.embed("red car")).unwrap() > 0.5);
}

/// A supplied `query_embedding` adds per-row cosine fused with the lexical leg; omitting one leaves every projected column as the lexical-only path. Without the vector backend it scores only rows the recency window recalled.
// spec: read.rank.caller-embedding@3845d68c
#[test]
fn a_caller_embedding_adds_a_cosine_leg_and_omitting_it_leaves_lexical_order() {
    let lexical = min_max(&[Some(3.0), Some(1.0)]);
    let rows = [[0.0f32, 1.0], [1.0, 0.0]];
    let lexical_only: Vec<f64> = lexical.iter().map(|l| fuse(None, *l)).collect();
    assert!(lexical_only[0] > lexical_only[1]);
    let query = [1.0f32, 0.0];
    let with_vector: Vec<f64> = rows.iter().zip(&lexical).map(|(r, l)| fuse(cosine(&query, r), *l)).collect();
    assert!(with_vector[1] > with_vector[0], "{with_vector:?}");
    assert_eq!(cosine(&query, &[1.0, 0.0, 0.0]), None, "a vector of another model's width contributes nothing");
}
