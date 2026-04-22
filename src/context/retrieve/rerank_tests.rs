use super::rerank::{rerank, RerankConfig, RerankInput};
use chrono::{DateTime, Duration, Utc};

fn input(
    id: &str,
    bm25: f32,
    vec: f32,
    id_match: bool,
    lang_match: bool,
    indexed: DateTime<Utc>,
) -> RerankInput {
    RerankInput {
        chunk_id: id.into(),
        bm25_raw: bm25,
        vec_raw: vec,
        id_exact_match: id_match,
        language_match: lang_match,
        indexed_at: indexed,
    }
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .unwrap()
        .with_timezone(&Utc)
}

fn now() -> DateTime<Utc> {
    t("2026-04-20T00:00:00Z")
}

#[test]
fn empty_inputs_returns_empty() {
    let config = RerankConfig::default();
    let out = rerank(&[], now(), &config);
    assert!(out.is_empty());
}

#[test]
fn single_candidate_gets_full_norm_when_has_signal() {
    let config = RerankConfig::default();
    let inputs = vec![input("a", 5.0, 0.8, false, false, now())];
    let out = rerank(&inputs, now(), &config);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].1.bm25_norm, 1.0);
    assert_eq!(out[0].1.vec_norm, 1.0);
}

#[test]
fn single_candidate_with_zero_raw_gets_zero_norm() {
    let config = RerankConfig::default();
    let inputs = vec![input("a", 0.0, 0.0, false, false, now())];
    let out = rerank(&inputs, now(), &config);
    assert_eq!(out[0].1.bm25_norm, 0.0);
    assert_eq!(out[0].1.vec_norm, 0.0);
    let blended = 0.6 * out[0].1.bm25_norm + 0.4 * out[0].1.vec_norm;
    assert_eq!(blended, 0.0);
}

#[test]
fn min_max_norm_across_two_candidates() {
    let config = RerankConfig::default();
    let inputs = vec![
        input("a", 10.0, 0.5, false, false, now()),
        input("b", 5.0, 0.3, false, false, now()),
    ];
    let out = rerank(&inputs, now(), &config);
    assert_eq!(out[0].1.bm25_norm, 1.0);
    assert_eq!(out[1].1.bm25_norm, 0.0);
    assert_eq!(out[0].1.vec_norm, 1.0);
    assert_eq!(out[1].1.vec_norm, 0.0);
}

#[test]
fn blend_weights_bm25_at_0_6_and_vec_at_0_4() {
    let config = RerankConfig::default();
    // A: max bm25, min vec -> (1.0, 0.0) -> blended 0.6
    // B: min bm25, max vec -> (0.0, 1.0) -> blended 0.4
    let inputs = vec![
        input("a", 10.0, 0.0, false, false, now()),
        input("b", 0.0, 1.0, false, false, now()),
    ];
    let out = rerank(&inputs, now(), &config);
    let a_score = out[0].1.score;
    let b_score = out[1].1.score;
    assert!((a_score - b_score).abs() - 0.2 < 1e-4);
}

#[test]
fn id_exact_match_adds_1_0_boost() {
    let config = RerankConfig::default();
    let inputs = vec![
        input("a", 5.0, 0.5, true, false, now()),
        input("b", 5.0, 0.5, false, false, now()),
    ];
    let out = rerank(&inputs, now(), &config);
    assert!((out[0].1.score - out[1].1.score - 1.0).abs() < 1e-5);
}

#[test]
fn language_match_adds_0_5_boost() {
    let config = RerankConfig::default();
    let inputs = vec![
        input("a", 5.0, 0.5, false, true, now()),
        input("b", 5.0, 0.5, false, false, now()),
    ];
    let out = rerank(&inputs, now(), &config);
    assert!((out[0].1.score - out[1].1.score - 0.5).abs() < 1e-5);
}

#[test]
fn recency_halves_at_halflife() {
    let config = RerankConfig {
        recency_halflife_days: 90,
        recency_floor: 0.0,
    };
    let n = now();
    let indexed = n - Duration::days(90);
    let inputs = vec![input("a", 5.0, 0.5, false, false, indexed)];
    let out = rerank(&inputs, n, &config);
    assert!((out[0].1.recency_mul - 0.5).abs() < 1e-4);
    // Single candidate -> blended = 1.0 -> score ~= 0.5
    assert!((out[0].1.score - 0.5).abs() < 1e-4);
}

#[test]
fn recency_floor_clamps_at_0_25() {
    let config = RerankConfig {
        recency_halflife_days: 90,
        recency_floor: 0.25,
    };
    let n = now();
    let indexed = n - Duration::days(1825);
    let inputs = vec![input("a", 5.0, 0.5, false, false, indexed)];
    let out = rerank(&inputs, n, &config);
    assert_eq!(out[0].1.recency_mul, 0.25);
}

#[test]
fn future_indexed_at_clamps_to_age_zero() {
    let config = RerankConfig::default();
    let n = now();
    let indexed = n + Duration::days(1);
    let inputs = vec![input("a", 5.0, 0.5, false, false, indexed)];
    let out = rerank(&inputs, n, &config);
    assert_eq!(out[0].1.recency_mul, 1.0);
}

#[test]
fn combined_example_matches_hand_computation() {
    let config = RerankConfig {
        recency_halflife_days: 90,
        recency_floor: 0.0,
    };
    let n = now();
    let indexed = n - Duration::days(30);
    let inputs = vec![
        input("a", 10.0, 0.8, true, true, indexed),
        input("b", 5.0, 0.4, false, false, indexed),
    ];
    let out = rerank(&inputs, n, &config);
    // decay = 2^(-1/3) ~= 0.7937005
    let expected_decay: f32 = (-std::f32::consts::LN_2 * 30.0 / 90.0).exp();
    let expected_a = (1.0_f32 + 1.0 + 0.5) * expected_decay;
    let expected_b = 0.0_f32 * expected_decay;
    assert!((out[0].1.score - expected_a).abs() < 1e-4);
    assert!((out[1].1.score - expected_b).abs() < 1e-4);
    assert!((expected_a - 1.9843).abs() < 1e-3);
}

#[test]
fn breakdown_components_sum_consistent() {
    let config = RerankConfig::default();
    let n = now();
    let inputs = vec![
        input("a", 10.0, 0.8, true, true, n - Duration::days(10)),
        input("b", 3.0, 0.4, false, true, n - Duration::days(45)),
        input("c", 0.0, 0.1, false, false, n - Duration::days(200)),
    ];
    let out = rerank(&inputs, n, &config);
    for (_, br) in &out {
        let reconstructed =
            (br.bm25_norm * 0.6 + br.vec_norm * 0.4 + br.id_boost + br.path_boost) * br.recency_mul;
        assert!(
            (reconstructed - br.score).abs() < 1e-5,
            "score mismatch: {} vs {}",
            reconstructed,
            br.score
        );
    }
}

#[test]
fn output_preserves_input_order() {
    let config = RerankConfig::default();
    let n = now();
    let inputs = vec![
        input("c", 1.0, 0.1, false, false, n),
        input("a", 2.0, 0.2, false, false, n),
        input("b", 3.0, 0.3, false, false, n),
    ];
    let out = rerank(&inputs, n, &config);
    let ids: Vec<&str> = out.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["c", "a", "b"]);
}

#[test]
fn recency_floor_respected_when_zero_raw_signal() {
    let config = RerankConfig {
        recency_halflife_days: 90,
        recency_floor: 0.25,
    };
    let n = now();
    let indexed = n - Duration::days(1825);
    let inputs = vec![input("a", 0.0, 0.0, false, false, indexed)];
    let out = rerank(&inputs, n, &config);
    assert_eq!(out[0].1.recency_mul, 0.25);
    assert_eq!(out[0].1.score, 0.0);
    assert!(out[0].1.score.is_finite());
}

#[test]
fn nan_raw_scores_become_zero() {
    let config = RerankConfig::default();
    let n = now();
    let inputs = vec![
        input("a", f32::NAN, 0.5, false, false, n),
        input("b", 5.0, f32::INFINITY, false, false, n),
        input("c", 1.0, 0.1, false, false, n),
    ];
    let out = rerank(&inputs, n, &config);
    for (_, br) in &out {
        assert!(br.score.is_finite(), "score must be finite");
        assert!(br.bm25_norm.is_finite());
        assert!(br.vec_norm.is_finite());
        assert!((0.0..=1.0).contains(&br.bm25_norm));
        assert!((0.0..=1.0).contains(&br.vec_norm));
    }
}

#[test]
fn recency_floor_above_one_is_clamped() {
    // recency_floor > 1.0 would otherwise amplify old items; rerank must
    // clamp it so recency_mul stays <= 1.0.
    let config = RerankConfig {
        recency_halflife_days: 90,
        recency_floor: 2.0,
    };
    let n = now();
    let indexed = n - Duration::days(3650);
    let inputs = vec![input("a", 5.0, 0.5, false, false, indexed)];
    let out = rerank(&inputs, n, &config);
    assert!(
        out[0].1.recency_mul <= 1.0,
        "recency_mul must be clamped: {}",
        out[0].1.recency_mul
    );
}

#[test]
fn negative_recency_floor_clamps_to_zero() {
    let config = RerankConfig {
        recency_halflife_days: 90,
        recency_floor: -0.5,
    };
    let n = now();
    let indexed = n - Duration::days(3650);
    let inputs = vec![input("a", 5.0, 0.5, false, false, indexed)];
    let out = rerank(&inputs, n, &config);
    assert!(out[0].1.recency_mul >= 0.0);
    assert!(out[0].1.recency_mul <= 1.0);
}
