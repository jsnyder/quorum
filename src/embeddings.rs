//! Local embedding model for semantic similarity.
//! Gated behind the `embeddings` Cargo feature.
//! Uses BAAI/bge-small-en-v1.5 via fastembed (ONNX Runtime).
//! Model auto-downloaded on first use, cached in ~/.quorum/models/

#[cfg(feature = "embeddings")]
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

#[cfg(feature = "embeddings")]
pub struct LocalEmbedder {
    model: TextEmbedding,
}

#[cfg(feature = "embeddings")]
impl LocalEmbedder {
    pub fn new() -> anyhow::Result<Self> {
        let mut options = InitOptions::default();
        options.model_name = EmbeddingModel::BGESmallENV15;
        options.show_download_progress = true;
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
        Ok(self.model.embed(texts.to_vec(), None)?)
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
    dot / (norm_a * norm_b)
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
        let mut embedder = LocalEmbedder::new().unwrap();
        let vec = embedder.embed("SQL injection in auth module").unwrap();
        assert_eq!(vec.len(), 384); // bge-small-en-v1.5 produces 384-dim
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn similar_texts_have_high_cosine() {
        let mut embedder = LocalEmbedder::new().unwrap();
        let a = embedder.embed("SQL injection vulnerability").unwrap();
        let b = embedder.embed("SQL injection in query").unwrap();
        let c = embedder.embed("Unused import os").unwrap();
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
            ab
        );
    }
}
