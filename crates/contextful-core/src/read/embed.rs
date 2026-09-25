//! `read.embed`: the embedding port, its deterministic default, and cosine similarity.

/// The embedding capability. An adapter reaching a model through the one inference
/// endpoint implements it beside the default.
pub trait Embedder {
    /// The identifier stored beside every vector this embedder produces.
    fn model(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// Dimensions of the default embedder's vectors.
pub const HASHING_DIMENSIONS: usize = 256;

/// The default embedder: token term frequencies hashed into signed buckets and
/// L2-normalized. It performs no I/O, downloads nothing and reads no key
/// (`read.embed.default-embedder`). It is a lexical-vector baseline: two texts sharing
/// no token are orthogonal, whatever they mean (`read.embed.default-embedder-reach`).
#[derive(Debug, Clone, Copy)]
pub struct HashingEmbedder {
    dimensions: usize,
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        HashingEmbedder { dimensions: HASHING_DIMENSIONS }
    }
}

/// FNV-1a over the token's bytes: stable across platforms, releases and processes.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

impl Embedder for HashingEmbedder {
    fn model(&self) -> &str {
        "contextful-hashing-256"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dimensions];
        for token in text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()) {
            let h = fnv1a(token.as_bytes());
            let bucket = (h % self.dimensions as u64) as usize;
            let sign = if (h >> 63) == 0 { 1.0 } else { -1.0 };
            v[bucket] += sign;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }
}

/// Cosine similarity, `None` for vectors of different dimensions or a zero vector.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
    let na: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    (na > 0.0 && nb > 0.0).then(|| dot / (na * nb))
}
