//! Local embedding model for semantic similarity.
//! Gated behind the `embeddings` Cargo feature.
//! Uses BAAI/bge-small-en-v1.5 via fastembed (ONNX Runtime).
//! Model auto-downloaded on first use, cached in ~/.quorum/models/

#[cfg(feature = "embeddings")]
use std::path::PathBuf;

#[cfg(feature = "embeddings")]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

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

/// Turns the embedder off without touching the Cargo feature (#565).
///
/// `TextEmbedding::try_new` downloads BAAI/bge-small-en-v1.5 from HuggingFace
/// when the cache is cold. That is a fifth outbound path, and unlike the other
/// four it is gated on no credential -- so `tests/support` had nothing to strip
/// and every `HOME`-isolated spawn attempted the download against a cold cache.
/// The harness sets this instead.
///
/// Also a real knob for anyone reviewing offline or on a metered link.
#[cfg(feature = "embeddings")]
pub const DISABLE_ENV: &str = "QUORUM_DISABLE_EMBEDDINGS";

/// How long to wait for model init before giving up and degrading (#565).
///
/// fastembed drives the download through `ureq` with no connect, read, or
/// overall deadline, and `TextInitOptions` exposes none, so a network stall parks
/// the caller forever -- measured once at 90 minutes across six test processes
/// before anyone noticed. The wait cannot be cancelled from here, but it can be
/// abandoned: `FeedbackIndex::build_hybrid` already falls back to BM25-only
/// when this returns `Err`, which is a far better outcome than hanging.
#[cfg(feature = "embeddings")]
const INIT_TIMEOUT_ENV: &str = "QUORUM_MODEL_INIT_TIMEOUT";
#[cfg(feature = "embeddings")]
const DEFAULT_INIT_TIMEOUT_SECS: u64 = 120;

/// Split from the env read so it is testable without mutating the process
/// environment, which #497 forbids in source for good reason: `set_var` is
/// unsafe in edition 2024 and tests share one process.
#[cfg(feature = "embeddings")]
fn init_timeout_from(raw: Option<&str>) -> std::time::Duration {
    let secs = raw
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_INIT_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

#[cfg(feature = "embeddings")]
fn init_timeout() -> std::time::Duration {
    init_timeout_from(std::env::var(INIT_TIMEOUT_ENV).ok().as_deref())
}

/// Latched when model init times out, so the cost is paid once per process.
///
/// Quorum's review of the first version pointed out that the abandoned thread
/// is per call, and `LocalEmbedder::new` runs per index build -- so a stalled
/// download on a ten-file review meant ten stuck threads and ten full 120s
/// waits. After the first timeout every later call fails fast into the same
/// BM25+Jaccard fallback.
///
/// Deliberately sticky: if the network recovers mid-run the process stays
/// degraded rather than re-gambling 120s per file. A review is short-lived and
/// degrading consistently beats degrading unpredictably.
#[cfg(feature = "embeddings")]
static INIT_TIMED_OUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "embeddings")]
fn disabled_from(raw: Option<&str>) -> bool {
    raw.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(feature = "embeddings")]
fn embeddings_disabled() -> bool {
    disabled_from(std::env::var(DISABLE_ENV).ok().as_deref())
}

#[cfg(feature = "embeddings")]
impl LocalEmbedder {
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with(embeddings_disabled(), init_timeout())
    }

    /// The constructor with its two environment decisions passed in, so a test
    /// can exercise the gate without `set_var` (#497).
    pub fn new_with(disabled: bool, deadline: std::time::Duration) -> anyhow::Result<Self> {
        if disabled {
            anyhow::bail!("embeddings disabled via {DISABLE_ENV}");
        }
        if INIT_TIMED_OUT.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("embedding model init already timed out in this process");
        }

        let mut options = TextInitOptions::default();
        options.model_name = EmbeddingModel::BGESmallENV15;
        options.show_download_progress = false;
        options.cache_dir = quorum_cache_dir();

        // Bounded wait. The thread is left detached on timeout rather than
        // joined: it is blocked in a syscall we cannot interrupt, and waiting
        // for it is the very thing being avoided. It holds no lock and writes
        // only into the fastembed cache, so abandoning it is safe.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(TextEmbedding::try_new(options));
        });
        match rx.recv_timeout(deadline) {
            Ok(Ok(model)) => Ok(Self { model }),
            Ok(Err(e)) => Err(anyhow::Error::from(e)),
            Err(_) => {
                INIT_TIMED_OUT.store(true, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    timeout_secs = deadline.as_secs(),
                    "embedding model init timed out (cold cache and a slow or \
                     stalled download?); continuing without embeddings"
                );
                anyhow::bail!("embedding model init timed out after {deadline:?}")
            }
        }
    }

    pub fn embed(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        let results = self.model.embed(vec![text], None)?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No embedding result"))
    }

    /// `embed_batch` through the vector cache for the corpus stored at
    /// `corpus`, kept beside it: one cache per corpus, so a test building an
    /// index from a temp store cannot overwrite the real one.
    pub fn embed_batch_cached(
        &mut self,
        corpus: &std::path::Path,
        texts: &[String],
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        embed_cached(&vector_cache_path(corpus), texts, |missing| {
            self.embed_batch(missing)
        })
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

/// Vectors for `texts`, reusing the ones stored at `path` by an earlier run and
/// calling `embed` only for texts it has not seen. Re-embedding the whole
/// feedback corpus was ~60 s of every review (6.6k entries, release build).
///
/// Keyed by the SHA-256 of each text, so an edited entry misses and a deleted
/// one drops out: the file is rewritten with exactly `texts`' vectors and never
/// outgrows the corpus. An unreadable or foreign file is an empty cache, and a
/// failed write only costs the next run its hits.
#[cfg(feature = "embeddings")]
pub fn embed_cached(
    path: &std::path::Path,
    texts: &[String],
    embed: impl FnOnce(&[String]) -> anyhow::Result<Vec<Vec<f32>>>,
) -> anyhow::Result<Vec<Vec<f32>>> {
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    let key = |t: &str| -> [u8; 32] { Sha256::digest(t.as_bytes()).into() };
    let mut cache: HashMap<[u8; 32], Vec<f32>> = read_vector_cache(path).unwrap_or_default();
    // Corpus order, not sorted: sorting made the cold batch ~30 s slower.
    let mut seen = std::collections::HashSet::new();
    let missing: Vec<String> = texts
        .iter()
        .filter(|t| !cache.contains_key(&key(t)) && seen.insert(key(t)))
        .cloned()
        .collect();
    let wrote_new = !missing.is_empty();
    if wrote_new {
        let fresh = embed(&missing)?;
        anyhow::ensure!(
            fresh.len() == missing.len(),
            "embedding batch size mismatch"
        );
        cache.extend(missing.iter().map(|t| key(t)).zip(fresh));
    }
    let keys: Vec<[u8; 32]> = texts.iter().map(|t| key(t)).collect();
    let out: Vec<Vec<f32>> = keys.iter().map(|k| cache[k].clone()).collect();
    let unique: std::collections::HashSet<_> = keys.iter().collect();
    // Rewrite only when something was added or a stored vector left the corpus.
    if (wrote_new || cache.len() != unique.len())
        && let Err(e) = write_vector_cache(path, keys.into_iter().zip(&out))
    {
        tracing::debug!(error = %e, "embedding cache not written");
    }
    Ok(out)
}

/// `~/.quorum/feedback.jsonl` -> `~/.quorum/feedback.jsonl.bge-small-en-v1.5.vectors`.
#[cfg(feature = "embeddings")]
pub fn vector_cache_path(corpus: &std::path::Path) -> PathBuf {
    let mut name = corpus.file_name().unwrap_or_default().to_os_string();
    name.push(".bge-small-en-v1.5.vectors");
    corpus.with_file_name(name)
}

#[cfg(feature = "embeddings")]
const VECTOR_CACHE_MAGIC: &[u8; 4] = b"QVC1";

/// `QVC1`, u32 dim, then per vector: 32-byte key, `dim` little-endian f32.
#[cfg(feature = "embeddings")]
fn read_vector_cache(
    path: &std::path::Path,
) -> Option<std::collections::HashMap<[u8; 32], Vec<f32>>> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 8 || &b[..4] != VECTOR_CACHE_MAGIC {
        return None;
    }
    let dim = u32::from_le_bytes(b[4..8].try_into().ok()?) as usize;
    // checked: `dim` comes from the file, and `dim * 4` can wrap on 32-bit.
    let rec = dim.checked_mul(4)?.checked_add(32)?;
    let body = &b[8..];
    if dim == 0 || body.len() % rec != 0 {
        return None;
    }
    Some(
        body.chunks_exact(rec)
            .map(|r| {
                let k: [u8; 32] = r[..32].try_into().expect("32-byte key");
                let v = r[32..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|f| f32::from_le_bytes(*f))
                    .collect();
                (k, v)
            })
            .collect(),
    )
}

#[cfg(feature = "embeddings")]
fn write_vector_cache<'a>(
    path: &std::path::Path,
    entries: impl Iterator<Item = ([u8; 32], &'a Vec<f32>)>,
) -> std::io::Result<()> {
    let mut buf = VECTOR_CACHE_MAGIC.to_vec();
    let mut dim = None;
    for (k, v) in entries {
        if *dim.get_or_insert(v.len()) != v.len() {
            return Err(std::io::Error::other("mixed vector dimensions"));
        }
        if buf.len() == 4 {
            buf.extend((v.len() as u32).to_le_bytes());
        }
        buf.extend(k);
        v.iter().for_each(|f| buf.extend(f.to_le_bytes()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, path)
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

    #[cfg(feature = "embeddings")]
    #[test]
    fn vector_cache_lives_beside_its_corpus() {
        let a = vector_cache_path(std::path::Path::new("/tmp/x/fb.jsonl"));
        let b = vector_cache_path(std::path::Path::new("/home/u/.quorum/feedback.jsonl"));
        assert_eq!(
            a,
            std::path::Path::new("/tmp/x/fb.jsonl.bge-small-en-v1.5.vectors")
        );
        assert_ne!(a.parent(), b.parent(), "two corpora must not share a cache");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn embed_cached_embeds_only_unseen_texts_and_drops_removed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.bin");
        fn fake(
            calls: &std::cell::RefCell<Vec<String>>,
        ) -> impl FnOnce(&[String]) -> anyhow::Result<Vec<Vec<f32>>> + '_ {
            move |m| {
                calls.borrow_mut().extend(m.iter().cloned());
                Ok(m.iter().map(|t| vec![t.len() as f32, 1.0]).collect())
            }
        }
        let s = |v: &[&str]| v.iter().map(|t| t.to_string()).collect::<Vec<_>>();

        let calls = std::cell::RefCell::new(vec![]);
        let first = embed_cached(&path, &s(&["a", "bb", "a"]), fake(&calls)).unwrap();
        assert_eq!(first, vec![vec![1.0, 1.0], vec![2.0, 1.0], vec![1.0, 1.0]]);
        assert_eq!(*calls.borrow(), s(&["a", "bb"]), "duplicates embed once");

        let calls = std::cell::RefCell::new(vec![]);
        let second = embed_cached(&path, &s(&["bb", "ccc"]), fake(&calls)).unwrap();
        assert_eq!(second, vec![vec![2.0, 1.0], vec![3.0, 1.0]]);
        assert_eq!(
            *calls.borrow(),
            s(&["ccc"]),
            "a cached text is not re-embedded"
        );

        // "a" was dropped from the file when it left the corpus.
        let calls = std::cell::RefCell::new(vec![]);
        embed_cached(&path, &s(&["a"]), fake(&calls)).unwrap();
        assert_eq!(*calls.borrow(), s(&["a"]));

        std::fs::write(&path, b"garbage").unwrap();
        let calls = std::cell::RefCell::new(vec![]);
        embed_cached(&path, &s(&["a"]), fake(&calls)).unwrap();
        assert_eq!(
            *calls.borrow(),
            s(&["a"]),
            "a corrupt file is an empty cache"
        );
    }

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

    /// #565: the gate stops the constructor before it can reach the network.
    #[cfg(feature = "embeddings")]
    #[test]
    fn the_gate_refuses_to_build_an_embedder() {
        let err = LocalEmbedder::new_with(true, std::time::Duration::from_secs(1))
            .err()
            .expect("the gate must refuse");
        assert!(
            err.to_string().contains(DISABLE_ENV),
            "the error should name the switch that caused it: {err}"
        );
    }

    /// #565: the deadline is configurable, bounded, and never zero. A zero or
    /// unparseable value must not become an instant timeout that silently
    /// disables embeddings for everyone.
    #[cfg(feature = "embeddings")]
    #[test]
    fn init_timeout_falls_back_to_the_default_on_junk() {
        assert_eq!(init_timeout_from(Some("45")).as_secs(), 45);
        for junk in [Some("0"), Some("not-a-number"), Some(""), None] {
            assert_eq!(
                init_timeout_from(junk).as_secs(),
                DEFAULT_INIT_TIMEOUT_SECS,
                "{junk:?} should fall back to the default"
            );
        }
    }

    /// #565: only an explicit truthy value disables. An empty or unrelated
    /// value left in the environment must not silently turn embeddings off.
    #[cfg(feature = "embeddings")]
    #[test]
    fn only_an_explicit_truthy_value_disables() {
        for on in [Some("1"), Some("true"), Some("TRUE")] {
            assert!(disabled_from(on), "{on:?} should disable");
        }
        for off in [Some("0"), Some(""), Some("no"), None] {
            assert!(!disabled_from(off), "{off:?} must not disable");
        }
    }
}
