//! Core traits for the context index: time + embeddings.
//!
//! `Clock` and `Embedder` are kept small so tests can inject deterministic
//! implementations. `HashEmbedder` is a stable, process-independent stand-in
//! used by index tests to avoid loading fastembed in CI.

use chrono::{DateTime, Utc};

/// Injectable time source for deterministic tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Wall-clock implementation.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Fixed clock for tests.
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

impl FixedClock {
    pub fn epoch() -> Self {
        Self(DateTime::<Utc>::from_timestamp(0, 0).unwrap())
    }

    pub fn from_rfc3339(s: &str) -> Self {
        Self(DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc))
    }
}

/// Text embedding interface. Implementations map text to fixed-dimensional
/// vectors; retrieval uses cosine similarity on these vectors.
pub trait Embedder: Send + Sync {
    /// Dimensionality of the output vector.
    fn dim(&self) -> usize;

    /// Embed a single text. Must be stable: same input produces same output
    /// within a process. Implementations SHOULD be deterministic across
    /// processes for a given model version; the `model_hash` identifies
    /// that version.
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Identifier recorded in state.json; a mismatch triggers a re-embed.
    /// Typical format: `"{impl-name}-{dim}-v{version}"`.
    fn model_hash(&self) -> String;
}

/// Deterministic test embedder — maps text to a fixed-dim vector by hashing
/// whitespace-split tokens and distributing their contributions across
/// dimensions. Same input always produces the same vector across processes.
/// NOT meant for production retrieval quality — purely a stable stand-in so
/// index tests don't load fastembed in CI.
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        assert!(dim > 0, "HashEmbedder dim must be positive");
        Self { dim }
    }
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut out = vec![0.0f32; self.dim];
        for token in text.split_whitespace() {
            let h = stable_hash(token.to_lowercase().as_bytes());
            let idx = (h % self.dim as u64) as usize;
            out[idx] += 1.0;
            let idx2 = ((h / 7 + 13) % self.dim as u64) as usize;
            out[idx2] += 0.5;
        }
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut out {
                *v /= norm;
            }
        }
        out
    }

    fn model_hash(&self) -> String {
        format!("hashembedder-{}-v1", self.dim)
    }
}

/// FNV-1a 64-bit. Fixed constants make this stable across Rust versions.
fn stable_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0001_0000_01b3);
    }
    h
}
