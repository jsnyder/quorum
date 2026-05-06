/// AST parse cache: avoid re-parsing unchanged files.
/// Keyed by SHA-256 hash of file content.
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use sha2::{Digest, Sha256};

use crate::parser::Language;

/// Cached parse result: the tree-sitter Tree and detected language.
pub struct CachedParse {
    pub tree: tree_sitter::Tree,
    pub lang: Language,
}

pub struct ParseCache {
    inner: Mutex<LruCache<String, CachedParse>>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
    capacity: usize,
}

impl ParseCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(NonZeroUsize::new(capacity.max(1)).unwrap())),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
            capacity,
        }
    }

    pub fn content_hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn get_or_parse(&self, content: &str, lang: Language) -> anyhow::Result<tree_sitter::Tree> {
        let hash = Self::content_hash(content);
        let mut cache = self.inner.lock().unwrap();

        if let Some(cached) = cache.get(&hash) {
            *self.hits.lock().unwrap() += 1;
            // tree-sitter Tree implements Clone
            return Ok(cached.tree.clone());
        }

        *self.misses.lock().unwrap() += 1;
        drop(cache); // release lock during parse

        let tree = crate::parser::parse(content, lang)?;
        let mut cache = self.inner.lock().unwrap();
        cache.put(
            hash,
            CachedParse {
                tree: tree.clone(),
                lang,
            },
        );
        Ok(tree)
    }

    pub fn stats(&self) -> CacheStats {
        let cache = self.inner.lock().unwrap();
        CacheStats {
            hits: *self.hits.lock().unwrap(),
            misses: *self.misses.lock().unwrap(),
            size: cache.len(),
            capacity: self.capacity,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub size: usize,
    pub capacity: usize,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_deterministic() {
        let h1 = ParseCache::content_hash("fn main() {}");
        let h2 = ParseCache::content_hash("fn main() {}");
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_hash_different_for_different_content() {
        let h1 = ParseCache::content_hash("fn main() {}");
        let h2 = ParseCache::content_hash("fn other() {}");
        assert_ne!(h1, h2);
    }

    #[test]
    fn cache_miss_then_hit() {
        let cache = ParseCache::new(10);
        let code = "fn main() {}";

        // First call: miss + parse
        let tree1 = cache.get_or_parse(code, Language::Rust).unwrap();
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);

        // Second call: hit
        let tree2 = cache.get_or_parse(code, Language::Rust).unwrap();
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);

        // Both produce valid trees
        assert_eq!(tree1.root_node().kind(), "source_file");
        assert_eq!(tree2.root_node().kind(), "source_file");
    }

    #[test]
    fn cache_miss_on_different_content() {
        let cache = ParseCache::new(10);
        cache.get_or_parse("fn a() {}", Language::Rust).unwrap();
        cache.get_or_parse("fn b() {}", Language::Rust).unwrap();
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().hits, 0);
    }

    #[test]
    fn cache_eviction_at_capacity() {
        let cache = ParseCache::new(2);
        cache.get_or_parse("fn a() {}", Language::Rust).unwrap();
        cache.get_or_parse("fn b() {}", Language::Rust).unwrap();
        cache.get_or_parse("fn c() {}", Language::Rust).unwrap(); // evicts a
        assert_eq!(cache.stats().size, 2);

        // "a" should be evicted, accessing it is a miss
        cache.get_or_parse("fn a() {}", Language::Rust).unwrap();
        assert_eq!(cache.stats().misses, 4); // a, b, c, a again
    }

    #[test]
    fn cache_stats_hit_rate() {
        let cache = ParseCache::new(10);
        let code = "fn x() {}";
        cache.get_or_parse(code, Language::Rust).unwrap(); // miss
        cache.get_or_parse(code, Language::Rust).unwrap(); // hit
        cache.get_or_parse(code, Language::Rust).unwrap(); // hit
        let stats = cache.stats();
        assert!((stats.hit_rate() - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn cache_empty_stats() {
        let cache = ParseCache::new(10);
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.size, 0);
        assert!((stats.hit_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cache_different_languages() {
        let cache = ParseCache::new(10);
        cache.get_or_parse("x = 1", Language::Python).unwrap();
        cache
            .get_or_parse("let x = 1;", Language::TypeScript)
            .unwrap();
        assert_eq!(cache.stats().size, 2);
    }
}
