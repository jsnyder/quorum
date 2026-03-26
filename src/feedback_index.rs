//! Feedback index for semantic precedent retrieval.
//! When embeddings feature is enabled, uses vector similarity.
//! Falls back to word Jaccard when embeddings not available.

use crate::feedback::{FeedbackEntry, FeedbackStore};
#[cfg(feature = "embeddings")]
use crate::patterns;

pub struct SimilarEntry {
    pub entry: FeedbackEntry,
    pub similarity: f32,
}

pub struct FeedbackIndex {
    entries: Vec<FeedbackEntry>,
    #[cfg(feature = "embeddings")]
    vectors: Vec<Vec<f32>>,
    #[cfg(feature = "embeddings")]
    embedder: Option<crate::embeddings::LocalEmbedder>,
}

impl FeedbackIndex {
    pub fn build(store: &FeedbackStore) -> anyhow::Result<Self> {
        let entries = store.load_all()?;

        #[cfg(feature = "embeddings")]
        {
            match crate::embeddings::LocalEmbedder::new() {
                Ok(mut embedder) => {
                    let texts: Vec<String> = entries.iter()
                        .map(|e| {
                            let pattern = patterns::classify_pattern(&e.finding_title, "", &e.finding_category);
                            patterns::embedding_text(&e.finding_title, &e.finding_category, pattern.as_deref())
                        })
                        .collect();
                    let vectors = if texts.is_empty() {
                        vec![]
                    } else {
                        embedder.embed_batch(&texts)?
                    };
                    eprintln!("FeedbackIndex: embedded {} entries with bge-small-en-v1.5", entries.len());
                    return Ok(Self { entries, vectors, embedder: Some(embedder) });
                }
                Err(e) => {
                    eprintln!("FeedbackIndex: embedding model unavailable ({}), using Jaccard fallback", e);
                }
            }
        }

        Ok(Self {
            entries,
            #[cfg(feature = "embeddings")]
            vectors: vec![],
            #[cfg(feature = "embeddings")]
            embedder: None,
        })
    }

    pub fn find_similar(&mut self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        #[cfg(feature = "embeddings")]
        if self.embedder.is_some() && !self.vectors.is_empty() {
            return self.find_similar_embedding(finding_title, category, top_k);
        }

        self.find_similar_jaccard(finding_title, category, top_k)
    }

    fn find_similar_jaccard(&self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        let mut scored: Vec<SimilarEntry> = self.entries.iter()
            .map(|e| {
                let title_sim = word_jaccard(finding_title, &e.finding_title);
                let cat_match = if !e.finding_category.is_empty() && category == e.finding_category { 0.4 } else { 0.0 };
                SimilarEntry { entry: e.clone(), similarity: (title_sim * 0.6 + cat_match) as f32 }
            })
            .collect();
        scored.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }

    #[cfg(feature = "embeddings")]
    fn find_similar_embedding(&mut self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        let pattern = patterns::classify_pattern(finding_title, "", category);
        let query_text = patterns::embedding_text(finding_title, category, pattern.as_deref());

        let query_vec = match self.embedder.as_mut().unwrap().embed(&query_text) {
            Ok(v) => v,
            Err(_) => return self.find_similar_jaccard(finding_title, category, top_k),
        };

        let mut scored: Vec<SimilarEntry> = self.entries.iter()
            .zip(self.vectors.iter())
            .map(|(entry, vec)| {
                let sim = crate::embeddings::cosine_similarity(&query_vec, vec);
                SimilarEntry { entry: entry.clone(), similarity: sim }
            })
            .collect();
        scored.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }
}

fn word_jaccard(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    let wa: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let wb: std::collections::HashSet<&str> = b.split_whitespace().collect();
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FeedbackStore, Verdict, Provenance};
    use tempfile::TempDir;
    use chrono::Utc;

    fn make_entry(title: &str, category: &str, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: title.into(),
            finding_category: category.into(),
            verdict,
            reason: "test".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: Provenance::Unknown,
        }
    }

    #[test]
    fn jaccard_retrieval_finds_similar() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        store.record(&make_entry("SQL injection in auth", "security", Verdict::Tp)).unwrap();
        store.record(&make_entry("Unused import os", "style", Verdict::Fp)).unwrap();
        store.record(&make_entry("SQL injection via f-string", "security", Verdict::Tp)).unwrap();

        let mut index = FeedbackIndex::build(&store).unwrap();
        let similar = index.find_similar("SQL injection in query", "security", 2);
        assert!(!similar.is_empty());
        assert!(similar[0].entry.finding_title.contains("SQL"));
    }

    #[test]
    fn empty_store_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let mut index = FeedbackIndex::build(&store).unwrap();
        let similar = index.find_similar("anything", "any", 5);
        assert!(similar.is_empty());
    }

    #[test]
    fn word_jaccard_identical_strings() {
        assert!((word_jaccard("SQL injection", "SQL injection") - 1.0).abs() < 0.001);
    }

    #[test]
    fn word_jaccard_empty_string() {
        assert_eq!(word_jaccard("", "something"), 0.0);
        assert_eq!(word_jaccard("something", ""), 0.0);
    }

    #[test]
    fn word_jaccard_partial_overlap() {
        let sim = word_jaccard("SQL injection in auth", "SQL injection via f-string");
        assert!(sim > 0.0);
        assert!(sim < 1.0);
    }
}
