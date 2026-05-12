//! Learned model for composite calibrator scoring.
//!
//! Lookup tables (word log-odds, family FP rates, language FP rates) and
//! feature weights computed by `quorum calibrate` from the feedback corpus.
//! At review time the calibrator loads this model and computes a composite
//! score that combines precedent weight, word-level signals, family-level
//! FP rates, and language-level FP rates.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

static RULE_PREFIX_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z0-9_-]+:\s*").unwrap());
static BACKTICK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"`[^`]+`").unwrap());
static NUMBER_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b\d+\b").unwrap());
static WHITESPACE_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\s+").unwrap());
pub static WORD_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[a-z_]+").unwrap());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    pub computed_at: String,
    pub feedback_count: usize,
    pub global_fp_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreWeights {
    pub score: f64,
    pub word_lor: f64,
    pub family_fp_inv: f64,
    pub language_fp_inv: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibratorModel {
    pub meta: ModelMeta,
    pub weights: ScoreWeights,
    pub word_lor: HashMap<String, f64>,
    pub family_fp_rate: HashMap<String, f64>,
    pub language_fp_rate: HashMap<String, f64>,
}

impl CalibratorModel {
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    pub fn load_from(path: &str) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Self::from_toml(&content).ok()
    }

    /// Normalize a finding title into a pattern family.
    ///
    /// Strips rule-id prefixes, replaces backtick-quoted identifiers with ``,
    /// replaces numbers with N, and collapses whitespace.
    pub fn title_family(title: &str) -> String {
        let mut t = title.to_lowercase();
        if let Some(m) = RULE_PREFIX_RE.find(&t) {
            t = t[m.end()..].to_string();
        }
        t = BACKTICK_RE.replace_all(&t, "``").to_string();
        t = NUMBER_RE.replace_all(&t, "N").to_string();
        t = WHITESPACE_RE.replace_all(&t, " ").to_string();
        t.trim().to_string()
    }

    /// Compute the average word log-odds ratio for a title.
    ///
    /// Tokenizes by extracting `[a-z_]+` runs, looks up each word in the
    /// vocabulary, and returns the mean. Unknown words contribute 0.0.
    pub fn word_lor_score(&self, title: &str) -> f64 {
        let lower = title.to_lowercase();
        let words: Vec<&str> = WORD_RE.find_iter(&lower).map(|m| m.as_str()).collect();
        if words.is_empty() {
            return 0.0;
        }
        let sum: f64 = words
            .iter()
            .map(|w| self.word_lor.get(*w).copied().unwrap_or(0.0))
            .sum();
        sum / words.len() as f64
    }

    /// Compute the composite score for a finding.
    ///
    /// Known families (present in `family_fp_rate`) use the family-specific
    /// FP rate; novel families fall back to `global_fp_rate`.
    pub fn composite_score(&self, precedent_score: f64, title: &str, file_ext_lang: &str) -> f64 {
        let family = Self::title_family(title);
        let family_fp = self
            .family_fp_rate
            .get(&family)
            .copied()
            .unwrap_or(self.meta.global_fp_rate);
        let lang_fp = self
            .language_fp_rate
            .get(file_ext_lang)
            .copied()
            .unwrap_or(self.meta.global_fp_rate);

        self.weights.score * precedent_score
            + self.weights.word_lor * self.word_lor_score(title)
            + self.weights.family_fp_inv * (1.0 - family_fp)
            + self.weights.language_fp_inv * (1.0 - lang_fp)
    }

    /// Map a file path to a language string for `language_fp_rate` lookup.
    ///
    /// Covers the same languages quorum supports for AST analysis. Extensions
    /// outside this set map to `"other"`, which may fall back to
    /// `global_fp_rate` if `other` has fewer than `LANG_MIN_SUPPORT` entries.
    pub fn file_ext_language(path: &str) -> &'static str {
        match std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
        {
            Some("rs") => "rust",
            Some("py") => "python",
            Some("ts" | "tsx") => "typescript",
            Some("js" | "jsx") => "javascript",
            Some("yaml" | "yml") => "yaml",
            Some("sh" | "bash" | "zsh") => "bash",
            Some("tf" | "tfvars") => "terraform",
            _ => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_model() -> CalibratorModel {
        CalibratorModel {
            meta: ModelMeta {
                computed_at: "2026-05-12T00:00:00Z".to_string(),
                feedback_count: 100,
                global_fp_rate: 0.27,
            },
            weights: ScoreWeights {
                score: 0.5,
                word_lor: 1.5,
                family_fp_inv: 1.0,
                language_fp_inv: 0.5,
            },
            word_lor: HashMap::from([
                ("hardcoded".into(), -1.88),
                ("secret".into(), -1.50),
                ("loop".into(), 1.47),
                ("function".into(), 0.20),
                ("complexity".into(), -0.80),
            ]),
            family_fp_rate: HashMap::from([(
                "function `` has cyclomatic complexity N".into(),
                0.30,
            )]),
            language_fp_rate: HashMap::from([("rust".into(), 0.328), ("python".into(), 0.208)]),
        }
    }

    #[test]
    fn round_trip_model() {
        let model = make_model();
        let toml_str = model.to_toml();
        let parsed = CalibratorModel::from_toml(&toml_str).unwrap();
        assert_eq!(parsed.word_lor.len(), 5);
        assert!((parsed.word_lor["hardcoded"] - (-1.88)).abs() < 1e-9);
        assert!((parsed.weights.word_lor - 1.5).abs() < 1e-9);
        assert!((parsed.meta.global_fp_rate - 0.27).abs() < 1e-9);
        assert_eq!(parsed.family_fp_rate.len(), 1);
        assert_eq!(parsed.language_fp_rate.len(), 2);
    }

    #[test]
    fn title_family_normalization() {
        assert_eq!(
            CalibratorModel::title_family("bare-except-pass: Function `process_data` has 42 lines"),
            "function `` has N lines"
        );
        assert_eq!(
            CalibratorModel::title_family("Hardcoded secret in `API_KEY`"),
            "hardcoded secret in ``"
        );
        assert_eq!(
            CalibratorModel::title_family("Simple title no prefix"),
            "simple title no prefix"
        );
        assert_eq!(
            CalibratorModel::title_family("rule-42: value is 100 or 200"),
            "value is N or N"
        );
    }

    #[test]
    fn title_family_empty_and_edge_cases() {
        assert_eq!(CalibratorModel::title_family(""), "");
        assert_eq!(CalibratorModel::title_family("   "), "");
        assert_eq!(CalibratorModel::title_family("`only_backtick`"), "``");
    }

    #[test]
    fn word_lor_scoring_known_words() {
        let model = make_model();
        // "hardcoded secret" -> avg(-1.88, -1.50) = -1.69
        let score = model.word_lor_score("hardcoded secret in `KEY`");
        // tokens: hardcoded, secret, in, key (in and key are unknown -> 0.0)
        // avg = (-1.88 + -1.50 + 0 + 0) / 4 = -0.845
        assert!((score - (-0.845)).abs() < 0.01);
    }

    #[test]
    fn word_lor_scoring_unknown_words() {
        let model = make_model();
        let score = model.word_lor_score("unknown stuff here");
        assert!((score - 0.0).abs() < 0.01);
    }

    #[test]
    fn word_lor_scoring_empty_title() {
        let model = make_model();
        assert!((model.word_lor_score("") - 0.0).abs() < 1e-9);
        assert!((model.word_lor_score("123 456") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn composite_score_known_family() {
        let model = make_model();
        let score =
            model.composite_score(0.8, "function `foo` has cyclomatic complexity 42", "rust");
        // title_family -> "function `` has cyclomatic complexity N"
        // family_fp = 0.30 (known)
        // lang_fp = 0.328 (rust)
        // word_lor tokens: function, foo, has, cyclomatic, complexity
        //   function=0.20, foo=0, has=0, cyclomatic=0, complexity=-0.80
        //   avg = (0.20 + 0 + 0 + 0 + -0.80) / 5 = -0.12
        // composite = 0.5*0.8 + 1.5*(-0.12) + 1.0*(1-0.30) + 0.5*(1-0.328)
        //           = 0.40 + (-0.18) + 0.70 + 0.336 = 1.256
        assert!((score - 1.256).abs() < 0.01);
    }

    #[test]
    fn composite_score_novel_family() {
        let model = make_model();
        let score = model.composite_score(0.8, "some totally new finding pattern", "python");
        // title_family -> "some totally new finding pattern" (not in family_fp_rate)
        // family_fp = global_fp_rate = 0.27 (fallback)
        // lang_fp = 0.208 (python)
        // word_lor tokens: some, totally, new, finding, pattern -> all unknown = 0
        // composite = 0.5*0.8 + 1.5*0.0 + 1.0*(1-0.27) + 0.5*(1-0.208)
        //           = 0.40 + 0.0 + 0.73 + 0.396 = 1.526
        assert!((score - 1.526).abs() < 0.01);
    }

    #[test]
    fn composite_score_unknown_language() {
        let model = make_model();
        let score_py = model.composite_score(0.5, "test finding", "python");
        let score_other = model.composite_score(0.5, "test finding", "other");
        // "other" falls back to global_fp_rate (0.27)
        // python uses 0.208
        // Difference: 0.5 * (0.208 - 0.27) = -0.031 (python slightly better)
        assert!((score_py - score_other).abs() < 0.1);
        assert!(score_py > score_other); // python has lower FP rate -> higher score
    }

    #[test]
    fn file_ext_language_mapping() {
        assert_eq!(CalibratorModel::file_ext_language("src/main.rs"), "rust");
        assert_eq!(
            CalibratorModel::file_ext_language("app/page.tsx"),
            "typescript"
        );
        assert_eq!(
            CalibratorModel::file_ext_language("app/page.ts"),
            "typescript"
        );
        assert_eq!(CalibratorModel::file_ext_language("config.yaml"), "yaml");
        assert_eq!(CalibratorModel::file_ext_language("script.sh"), "bash");
        assert_eq!(CalibratorModel::file_ext_language("main.py"), "python");
        assert_eq!(CalibratorModel::file_ext_language("index.js"), "javascript");
        assert_eq!(CalibratorModel::file_ext_language("main.tf"), "terraform");
        assert_eq!(CalibratorModel::file_ext_language("Dockerfile"), "other");
    }

    #[test]
    fn load_from_missing_returns_none() {
        assert!(CalibratorModel::load_from("/nonexistent/path").is_none());
    }
}
