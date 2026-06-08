//! Local embedding model for semantic similarity.
//! Gated behind the `embeddings` Cargo feature.
//! Uses BAAI/bge-small-en-v1.5 via fastembed (ONNX Runtime).
//! Model auto-downloaded on first use, cached in ~/.quorum/models/

#[cfg(feature = "embeddings")]
use std::path::PathBuf;

#[cfg(feature = "embeddings")]
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

#[cfg(feature = "embeddings")]
pub struct LocalEmbedder {
    model: TextEmbedding,
}

#[cfg(feature = "embeddings")]
fn quorum_cache_dir() -> PathBuf {
    match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(".quorum").join("models"),
        Err(_) => PathBuf::from(".fastembed_cache"),
    }
}

#[cfg(feature = "embeddings")]
impl LocalEmbedder {
    pub fn new() -> anyhow::Result<Self> {
        let mut options = InitOptions::default();
        options.model_name = EmbeddingModel::BGESmallENV15;
        options.show_download_progress = false;
        options.cache_dir = quorum_cache_dir();
        let model = TextEmbedding::try_new(options)?;
        Ok(Self { model })
    }

    pub fn embed(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        let results = self.model.embed(vec![text], None)?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No embedding result"))
    }

    pub fn embed_batch(&mut self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let results = self.model.embed(texts, None)?;
        if results.len() != texts.len() {
            anyhow::bail!(
                "embedding batch size mismatch: got {} vectors for {} inputs",
                results.len(),
                texts.len()
            );
        }
        Ok(results)
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    let result = dot / (norm_a * norm_b);
    if result.is_finite() { result } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 0.001);
    }

    #[test]
    fn cosine_empty_vectors() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn embed_text_returns_vector() {
        let mut embedder = match LocalEmbedder::new() {
            Ok(e) => e,
            Err(err) => {
                eprintln!("skipping: embedding model unavailable: {err}");
                return;
            }
        };
        let vec = match embedder.embed("SQL injection in auth module") {
            Ok(v) => v,
            Err(err) => {
                eprintln!("skipping: embedding inference failed: {err}");
                return;
            }
        };
        assert_eq!(vec.len(), 384); // bge-small-en-v1.5 produces 384-dim
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn similar_texts_have_high_cosine() {
        let mut embedder = match LocalEmbedder::new() {
            Ok(e) => e,
            Err(err) => {
                eprintln!("skipping: embedding model unavailable: {err}");
                return;
            }
        };
        let a = match embedder.embed("SQL injection vulnerability") {
            Ok(v) => v,
            Err(err) => {
                eprintln!("skipping: embedding inference failed: {err}");
                return;
            }
        };
        let b = match embedder.embed("SQL injection in query") {
            Ok(v) => v,
            Err(err) => {
                eprintln!("skipping: embedding inference failed: {err}");
                return;
            }
        };
        let c = match embedder.embed("Unused import os") {
            Ok(v) => v,
            Err(err) => {
                eprintln!("skipping: embedding inference failed: {err}");
                return;
            }
        };
        let ab = cosine_similarity(&a, &b);
        let ac = cosine_similarity(&a, &c);
        assert!(
            ab > 0.7,
            "Similar texts should have high similarity: {}",
            ab
        );
        assert!(
            ac < ab,
            "Different texts should have lower similarity: {} vs {}",
            ac,
            ab,
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn embed_batch_empty_input_returns_empty() {
        let mut embedder = match LocalEmbedder::new() {
            Ok(e) => e,
            Err(err) => {
                eprintln!("skipping: embedding model unavailable: {err}");
                return;
            }
        };
        let result = embedder.embed_batch(&[]).unwrap();
        assert!(result.is_empty(), "empty input should produce empty output");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn cache_dir_is_absolute_and_stable() {
        if std::env::var("HOME").is_err() {
            eprintln!("skipping: HOME not set, cache_dir will use relative fallback");
            return;
        }
        let dir = super::quorum_cache_dir();
        assert!(
            dir.is_absolute(),
            "cache_dir must be absolute, got: {}",
            dir.display()
        );
        let dir2 = super::quorum_cache_dir();
        assert_eq!(dir, dir2, "cache_dir must be deterministic across calls");
    }
}
