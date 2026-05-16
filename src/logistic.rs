/// Inline L2-regularized logistic regression.
///
/// No external dependencies — pure Rust implementation using batch gradient
/// descent with backtracking line search (Armijo condition).
const EPSILON: f64 = 1e-8;
const ARMIJO_C: f64 = 1e-4;
const ARMIJO_BETA: f64 = 0.5;

/// Numerically stable sigmoid function.
///
/// Branches on the sign of `z` to avoid overflow in `exp()`.
pub fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        let e = (-z).exp();
        1.0 / (1.0 + e)
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// Z-score normalization of feature matrix.
///
/// Returns `(normalized_data, means, stddevs)`. Uses population stddev
/// (divide by N) with an epsilon of 1e-8 to prevent division by zero on
/// constant columns.
pub fn standardize(data: &[Vec<f64>]) -> (Vec<Vec<f64>>, Vec<f64>, Vec<f64>) {
    let n = data.len();
    assert!(n > 0, "standardize requires at least one sample");
    let p = data[0].len();

    let mut means = vec![0.0; p];
    for row in data {
        for (j, val) in row.iter().enumerate() {
            means[j] += val;
        }
    }
    for m in &mut means {
        *m /= n as f64;
    }

    let mut stddevs = vec![0.0; p];
    for row in data {
        for (j, val) in row.iter().enumerate() {
            let diff = val - means[j];
            stddevs[j] += diff * diff;
        }
    }
    for s in &mut stddevs {
        *s = (*s / n as f64).sqrt();
    }

    let normed: Vec<Vec<f64>> = data
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(j, val)| (val - means[j]) / (stddevs[j] + EPSILON))
                .collect()
        })
        .collect();

    (normed, means, stddevs)
}

/// Result of logistic regression fitting.
pub struct LogisticFit {
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub feature_means: Vec<f64>,
    pub feature_stddevs: Vec<f64>,
}

impl LogisticFit {
    /// Predict P(positive) for a single raw (unstandardized) sample.
    pub fn predict_one(&self, x: &[f64]) -> f64 {
        let z: f64 = x
            .iter()
            .enumerate()
            .map(|(j, val)| {
                let normed = (val - self.feature_means[j])
                    / (self.feature_stddevs[j] + EPSILON);
                normed * self.coefficients[j]
            })
            .sum::<f64>()
            + self.intercept;
        sigmoid(z)
    }

    /// Batch prediction on raw (unstandardized) data.
    pub fn predict(&self, data: &[Vec<f64>]) -> Vec<f64> {
        data.iter().map(|row| self.predict_one(row)).collect()
    }
}

/// Numerically stable cross-entropy loss for a single sample.
///
/// Formula: `-y*z + max(z, 0) + ln(1 + exp(-|z|))`
fn cross_entropy_single(z: f64, y: f64) -> f64 {
    -y * z + z.max(0.0) + (1.0 + (-z.abs()).exp()).ln()
}

/// Compute total loss (cross-entropy + L2 penalty on weights).
fn loss(
    x_norm: &[Vec<f64>],
    y: &[f64],
    weights: &[f64],
    intercept: f64,
    lambda: f64,
) -> f64 {
    let n = x_norm.len() as f64;
    let ce: f64 = x_norm
        .iter()
        .zip(y.iter())
        .map(|(xi, &yi)| {
            let z: f64 =
                xi.iter().zip(weights.iter()).map(|(a, b)| a * b).sum::<f64>()
                    + intercept;
            cross_entropy_single(z, yi)
        })
        .sum();
    let l2: f64 = weights.iter().map(|w| w * w).sum::<f64>();
    ce / n + 0.5 * lambda * l2
}

/// Fit L2-regularized logistic regression via batch gradient descent with
/// backtracking line search (Armijo condition).
///
/// `x` is the raw feature matrix, `y` is the binary label vector, `lambda`
/// is the L2 regularization strength, and `max_iter` is the iteration cap.
pub fn fit(x: &[Vec<f64>], y: &[bool], lambda: f64, max_iter: usize) -> LogisticFit {
    assert!(!x.is_empty(), "fit requires at least one sample");
    let p = x[0].len();
    let n = x.len();

    // Standardize features
    let (x_norm, means, stddevs) = standardize(x);

    // Convert labels to f64
    let y_f: Vec<f64> = y.iter().map(|&b| if b { 1.0 } else { 0.0 }).collect();

    // Initialize weights and intercept to zero
    let mut weights = vec![0.0; p];
    let mut intercept = 0.0;

    let mut prev_loss = loss(&x_norm, &y_f, &weights, intercept, lambda);

    for _ in 0..max_iter {
        // Compute gradient
        let mut grad_w = vec![0.0; p];
        let mut grad_b = 0.0;

        for (xi, &yi) in x_norm.iter().zip(y_f.iter()) {
            let z: f64 =
                xi.iter().zip(weights.iter()).map(|(a, b)| a * b).sum::<f64>()
                    + intercept;
            let residual = sigmoid(z) - yi;
            for (j, xij) in xi.iter().enumerate() {
                grad_w[j] += residual * xij;
            }
            grad_b += residual;
        }

        let n_f = n as f64;
        for (j, gw) in grad_w.iter_mut().enumerate() {
            *gw = *gw / n_f + lambda * weights[j];
        }
        grad_b /= n_f;

        // Compute squared gradient norm for Armijo condition
        let grad_norm_sq: f64 = grad_w.iter().map(|g| g * g).sum::<f64>()
            + grad_b * grad_b;

        // Backtracking line search
        let mut step = 1.0;
        let mut new_weights: Vec<f64>;
        let mut new_intercept: f64;
        let mut new_loss: f64;

        for _ in 0..20 {
            new_weights = weights
                .iter()
                .zip(grad_w.iter())
                .map(|(w, g)| w - step * g)
                .collect();
            new_intercept = intercept - step * grad_b;
            new_loss = loss(&x_norm, &y_f, &new_weights, new_intercept, lambda);

            if new_loss <= prev_loss - ARMIJO_C * step * grad_norm_sq {
                weights = new_weights;
                intercept = new_intercept;
                prev_loss = new_loss;
                break;
            }
            step *= ARMIJO_BETA;

            // If we exhaust line search iterations, take the last step
            if step < 1e-15 {
                weights = weights
                    .iter()
                    .zip(grad_w.iter())
                    .map(|(w, g)| w - step * g)
                    .collect();
                intercept -= step * grad_b;
                prev_loss = loss(&x_norm, &y_f, &weights, intercept, lambda);
                break;
            }
        }

        // Check convergence: relative loss change
        let curr_loss = loss(&x_norm, &y_f, &weights, intercept, lambda);
        let rel_change = (prev_loss - curr_loss).abs() / (prev_loss.abs() + EPSILON);
        prev_loss = curr_loss;

        if rel_change < 1e-4 {
            break;
        }
    }

    LogisticFit {
        coefficients: weights,
        intercept,
        feature_means: means,
        feature_stddevs: stddevs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_at_zero() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sigmoid_extreme_positive() {
        assert!((sigmoid(100.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sigmoid_extreme_negative() {
        assert!(sigmoid(-100.0) < 1e-6);
    }

    #[test]
    fn standardize_basic() {
        let data = vec![vec![1.0, 10.0], vec![3.0, 20.0], vec![5.0, 30.0]];
        let (normed, means, stddevs) = standardize(&data);
        assert!((means[0] - 3.0).abs() < 1e-9);
        assert!((means[1] - 20.0).abs() < 1e-9);
        // Normalized col 0 should have mean ~0
        let col0_mean: f64 = normed.iter().map(|r| r[0]).sum::<f64>() / 3.0;
        assert!(col0_mean.abs() < 1e-9);
        // Verify stddev is population stddev
        // stddev of [1,3,5] = sqrt(((1-3)^2 + (3-3)^2 + (5-3)^2)/3) = sqrt(8/3)
        let expected_std = (8.0_f64 / 3.0).sqrt();
        assert!(
            (stddevs[0] - expected_std).abs() < 1e-9,
            "Expected stddev {expected_std}, got {}",
            stddevs[0]
        );
    }

    #[test]
    fn standardize_constant_column() {
        let data = vec![vec![5.0, 1.0], vec![5.0, 2.0], vec![5.0, 3.0]];
        let (normed, _, stddevs) = standardize(&data);
        assert!(normed[0][0].is_finite()); // epsilon prevents NaN
        assert!(stddevs[0] < 1e-6); // constant -> near-zero stddev
    }

    #[test]
    fn fit_linearly_separable() {
        // 100 samples: feature goes 0.0..1.0, label flips at 0.5
        let x: Vec<Vec<f64>> = (0..100).map(|i| vec![i as f64 / 100.0]).collect();
        let y: Vec<bool> = (0..100).map(|i| i >= 50).collect();
        let result = fit(&x, &y, 0.1, 500);
        let correct = x
            .iter()
            .zip(y.iter())
            .filter(|(xi, yi)| (result.predict_one(xi) >= 0.5) == **yi)
            .count();
        assert!(correct >= 90, "Expected >=90% accuracy, got {correct}/100");
    }

    #[test]
    fn fit_two_features() {
        // Linearly separable: label = (a + b > 0.7)
        let x: Vec<Vec<f64>> = (0..200)
            .map(|i| {
                let a = (i % 10) as f64 / 10.0;
                let b = (i / 10) as f64 / 20.0;
                vec![a, b]
            })
            .collect();
        let y: Vec<bool> = x.iter().map(|row| row[0] + row[1] > 0.7).collect();
        let result = fit(&x, &y, 0.1, 500);
        let correct = x
            .iter()
            .zip(y.iter())
            .filter(|(xi, yi)| (result.predict_one(xi) >= 0.5) == **yi)
            .count();
        assert!(
            correct >= 170,
            "Expected >=85% accuracy on 2-feature, got {correct}/200"
        );
    }

    #[test]
    fn predict_batch_matches_individual() {
        let x: Vec<Vec<f64>> = (0..50).map(|i| vec![i as f64 / 50.0]).collect();
        let y: Vec<bool> = (0..50).map(|i| i >= 25).collect();
        let model = fit(&x, &y, 0.1, 500);
        let batch = model.predict(&x);
        for (i, xi) in x.iter().enumerate() {
            assert!((batch[i] - model.predict_one(xi)).abs() < 1e-12);
        }
    }
}
