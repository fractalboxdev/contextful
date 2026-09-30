//! The deterministic tier's stub embedder (`assurance.evaluate.stub-embedder`): a seeded
//! feature-hashing embedding, so the vector leg ranks without a model call and a replay of
//! one seed reproduces every vector.
//!
//! An ASCII word of two or more characters is one feature, its trailing `s` dropped from
//! four characters up; a run holding any non-ASCII character contributes its character
//! bigrams, since no word boundary occurs beside an ideograph. Each feature hashes, under
//! the seed, to one signed dimension, and the vector is unit length.

/// Dimensions of a stub embedding.
pub const STUB_DIM: usize = 128;

/// The seed a run's stub embedder derives from when the caller names none.
pub const DEFAULT_SEED: u64 = 0x5eed_0078;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StubEmbedder {
    pub seed: u64,
    pub dim: usize,
}

impl StubEmbedder {
    pub fn new(seed: u64) -> StubEmbedder {
        StubEmbedder { seed, dim: STUB_DIM }
    }

    /// The unit-length embedding of `text`; the zero vector when it holds no feature.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for feature in features(text) {
            let h = fnv1a(self.seed, feature.as_bytes());
            let slot = (h % self.dim as u64) as usize;
            v[slot] += if (h >> 63) == 1 { -1.0 } else { 1.0 };
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }
}

/// The features of `text`, in order, repeats kept.
pub fn features(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for run in lower.split(|c: char| !c.is_alphanumeric()).filter(|r| !r.is_empty()) {
        if run.is_ascii() {
            if run.len() < 2 {
                continue;
            }
            let word = if run.len() >= 4 { run.strip_suffix('s').unwrap_or(run) } else { run };
            out.push(word.to_string());
        } else {
            let chars: Vec<char> = run.chars().collect();
            if chars.len() == 1 {
                out.push(run.to_string());
            }
            out.extend(chars.windows(2).map(|w| w.iter().collect::<String>()));
        }
    }
    out
}

/// FNV-1a over the seed's bytes, then `bytes`.
fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.to_le_bytes().iter().chain(bytes) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

