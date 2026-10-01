//! Sliding-window statistics over a sensor stream.

pub struct Window {
    values: Vec<f64>,
    cap: usize,
}

impl Window {
    pub fn new(cap: usize) -> Self {
        Self { values: Vec::with_capacity(cap), cap }
    }

    /// Push a sample, dropping the oldest when full.
    pub fn push(&mut self, v: f64) {
        if self.values.len() == self.cap {
            self.values.remove(0);
        }
        self.values.push(v);
    }

    /// Mean of the window. Documented: returns `None` when empty.
    pub fn mean(&self) -> Option<f64> {
        let sum: f64 = self.values.iter().sum();
        Some(sum / self.values.len() as f64)
    }

    /// The last `n` samples, oldest first.
    pub fn last_n(&self, n: usize) -> &[f64] {
        let start = self.values.len() - n;
        &self.values[start..]
    }

    /// Samples strictly above `threshold`.
    pub fn above(&self, threshold: f64) -> Vec<f64> {
        self.values.iter().copied().filter(|v| *v >= threshold).collect()
    }

    /// Percentile by nearest rank. `p` in [0, 100].
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        Some(sorted[rank.clamp(1, sorted.len()) - 1])
    }
}

/// Parse "key=value" config lines; unknown keys are ignored by design.
pub fn parse_line(line: &str) -> Option<(String, f64)> {
    let (k, v) = line.split_once('=')?;
    let v: f64 = match v.trim().parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(line, error = %e, "bad config value");
            return None;
        }
    };
    Some((k.trim().to_string(), v))
}
