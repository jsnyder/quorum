//! Threshold configuration for data-driven calibrator tuning.
//!
//! Stores computed suppress/boost thresholds in TOML format at
//! `~/.quorum/calibrator_thresholds.toml`. The calibrator reads this
//! at startup and falls back to legacy behavior when no file exists.

use serde::{Deserialize, Serialize};

/// A single path's computed threshold (suppress or boost).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathThreshold {
    /// The target precision used to derive this threshold.
    pub precision_target: f64,
    /// The score cutoff. For suppress: suppress when score < threshold.
    /// For boost: boost when score >= threshold.
    pub threshold: f64,
}

/// Top-level threshold configuration written to/read from TOML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThresholdConfig {
    /// Suppress path: findings scoring below this threshold are fully suppressed.
    pub suppress: Option<PathThreshold>,
    /// Boost path: findings scoring at or above this threshold get severity boosted.
    pub boost: Option<PathThreshold>,
}

impl ThresholdConfig {
    /// Serialize to a TOML string.
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Deserialize from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not valid TOML or does not match
    /// the expected schema.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load from a file path. Returns `None` if the file does not exist or
    /// is malformed (logs a warning on malformed content).
    pub fn load_from(path: &str) -> Option<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(path, error = %e, "failed to read calibrator_thresholds.toml");
                return None;
            }
        };
        match Self::from_toml(&content) {
            Ok(config) => config.validate(),
            Err(e) => {
                tracing::warn!(
                    path,
                    error = %e,
                    "malformed calibrator_thresholds.toml, using defaults"
                );
                None
            }
        }
    }

    fn validate(self) -> Option<Self> {
        let valid = |p: &PathThreshold| {
            p.precision_target.is_finite()
                && p.threshold.is_finite()
                && (0.0..=1.0).contains(&p.precision_target)
                && (0.0..=1.0).contains(&p.threshold)
        };
        if self.suppress.as_ref().is_some_and(|p| !valid(p))
            || self.boost.as_ref().is_some_and(|p| !valid(p))
        {
            tracing::warn!("calibrator_thresholds.toml contains out-of-range values, using defaults");
            return None;
        }
        if let (Some(s), Some(b)) = (&self.suppress, &self.boost)
            && s.threshold >= b.threshold
        {
            tracing::warn!(
                suppress = s.threshold,
                boost = b.threshold,
                "calibrator_thresholds.toml: suppress >= boost, using defaults"
            );
            return None;
        }
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_full_config() {
        let config = ThresholdConfig {
            suppress: Some(PathThreshold {
                precision_target: 0.95,
                threshold: 0.78,
            }),
            boost: Some(PathThreshold {
                precision_target: 0.85,
                threshold: 0.42,
            }),
        };
        let toml_str = config.to_toml();
        let parsed = ThresholdConfig::from_toml(&toml_str).unwrap();
        assert!((parsed.suppress.unwrap().threshold - 0.78).abs() < 1e-9);
        assert!((parsed.boost.unwrap().threshold - 0.42).abs() < 1e-9);
    }

    #[test]
    fn partial_config_only_boost() {
        let toml_str = "[boost]\nprecision_target = 0.85\nthreshold = 0.42\n";
        let parsed = ThresholdConfig::from_toml(toml_str).unwrap();
        assert!(parsed.suppress.is_none());
        assert!(parsed.boost.is_some());
    }

    #[test]
    fn malformed_toml_returns_error() {
        let result = ThresholdConfig::from_toml("not valid [[[toml");
        assert!(result.is_err());
    }

    #[test]
    fn read_from_missing_file_returns_none() {
        let result = ThresholdConfig::load_from("/nonexistent/path/thresholds.toml");
        assert!(result.is_none());
    }

    #[test]
    fn validate_rejects_out_of_range_threshold() {
        let config = ThresholdConfig {
            suppress: Some(PathThreshold {
                precision_target: 0.95,
                threshold: 1.5, // out of range
            }),
            boost: None,
        };
        assert!(config.validate().is_none());
    }

    #[test]
    fn validate_rejects_suppress_gte_boost() {
        let config = ThresholdConfig {
            suppress: Some(PathThreshold {
                precision_target: 0.95,
                threshold: 0.8,
            }),
            boost: Some(PathThreshold {
                precision_target: 0.85,
                threshold: 0.5, // suppress >= boost
            }),
        };
        assert!(config.validate().is_none());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let config = ThresholdConfig {
            suppress: Some(PathThreshold {
                precision_target: 0.95,
                threshold: 0.3,
            }),
            boost: Some(PathThreshold {
                precision_target: 0.85,
                threshold: 0.7,
            }),
        };
        assert!(config.validate().is_some());
    }
}
