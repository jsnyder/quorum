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
    /// Construct a `HashEmbedder` with a positive dimension.
    ///
    /// Returns `Err` when `dim == 0`. Prefer this in any path that derives
    /// `dim` from configuration or user input; callers with a compile-time
    /// constant can use [`HashEmbedder::new`], which forwards to this and
    /// panics on failure.
    pub fn try_new(dim: usize) -> anyhow::Result<Self> {
        if dim == 0 {
            anyhow::bail!("HashEmbedder dim must be positive");
        }
        Ok(Self { dim })
    }

    pub fn new(dim: usize) -> Self {
        Self::try_new(dim).expect("HashEmbedder dim must be positive")
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

/// Production embedder backed by fastembed's bge-small-en-v1.5 (384-dim).
///
/// `fastembed::TextEmbedding::embed` takes `&mut self`, but the `Embedder`
/// trait requires `&self + Send + Sync` so it can be shared across the
/// pipeline. We serialize access with a `Mutex`; inference is CPU-bound
/// and batched at a higher layer (`IndexBuilder::rebuild_from_jsonl`), so
/// the lock is not on the hot retrieval path for reviews — only during
/// indexing and the per-query embed in `RetrieverFn`, which happens once
/// per review.
#[cfg(feature = "embeddings")]
pub struct FastEmbedEmbedder {
    inner: std::sync::Mutex<crate::embeddings::LocalEmbedder>,
}

#[cfg(feature = "embeddings")]
impl FastEmbedEmbedder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: std::sync::Mutex::new(crate::embeddings::LocalEmbedder::new()?),
        })
    }
}

#[cfg(feature = "embeddings")]
impl Embedder for FastEmbedEmbedder {
    fn dim(&self) -> usize {
        // BGE-small-en-v1.5 is fixed at 384 dimensions.
        384
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // Fall back to a zero vector on failure: the index is still
        // usable via BM25 alone, so a transient embed error shouldn't
        // crash the review. A persistent failure is obvious downstream
        // (every vec0 query returns degenerate similarities).
        match self.inner.lock() {
            Ok(mut m) => m.embed(text).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "fastembed embed failed; returning zero vector");
                vec![0.0; self.dim()]
            }),
            Err(_) => {
                tracing::warn!("fastembed mutex poisoned; returning zero vector");
                vec![0.0; self.dim()]
            }
        }
    }

    fn model_hash(&self) -> String {
        // Matching this string against the one persisted in `state.json`
        // drives the re-embed check in `IndexBuilder::requires_reembedding`.
        // Bump the suffix when we change embedding models.
        "fastembed-bge-small-en-v1.5-384".into()
    }
}
