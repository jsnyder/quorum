use super::traits::{Clock, Embedder, FixedClock, HashEmbedder};
use chrono::{Datelike, Timelike};

#[test]
fn hash_embedder_produces_same_vector_for_same_input() {
    let e = HashEmbedder::new(384);
    let v1 = e.embed("verify token jwt");
    let v2 = e.embed("verify token jwt");
    assert_eq!(v1, v2);
}

#[test]
fn hash_embedder_produces_different_vectors_for_different_inputs() {
    let e = HashEmbedder::new(384);
    let v1 = e.embed("verify token");
    let v2 = e.embed("database query");
    assert_ne!(v1, v2);
}

#[test]
fn hash_embedder_l2_normalized() {
    let e = HashEmbedder::new(384);
    let v = e.embed("some text here");
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5);
}

#[test]
fn hash_embedder_dim_correct() {
    let e = HashEmbedder::new(384);
    assert_eq!(e.embed("x").len(), 384);
    assert_eq!(e.dim(), 384);
}

#[test]
fn hash_embedder_empty_returns_zero_vector() {
    let e = HashEmbedder::new(384);
    let v = e.embed("");
    assert_eq!(v.len(), 384);
    assert!(v.iter().all(|x| *x == 0.0));
}

#[test]
fn hash_embedder_model_hash_changes_with_dim() {
    assert_ne!(
        HashEmbedder::new(384).model_hash(),
        HashEmbedder::new(512).model_hash()
    );
}

#[test]
fn fixed_clock_epoch_returns_1970() {
    let c = FixedClock::epoch();
    let now = c.now();
    assert_eq!(now.year(), 1970);
    assert_eq!(now.month(), 1);
    assert_eq!(now.day(), 1);
    assert_eq!(now.hour(), 0);
    assert_eq!(now.minute(), 0);
    assert_eq!(now.second(), 0);
}

#[test]
fn fixed_clock_from_rfc3339() {
    let c = FixedClock::from_rfc3339("2026-04-20T12:00:00Z");
    let now = c.now();
    assert_eq!(now.year(), 2026);
    assert_eq!(now.month(), 4);
    assert_eq!(now.day(), 20);
    assert_eq!(now.hour(), 12);
}

#[test]
fn hash_embedder_case_insensitive() {
    let e = HashEmbedder::new(384);
    assert_eq!(e.embed("Token"), e.embed("token"));
}

#[test]
fn dispatch_reexports_still_work() {
    use crate::context::extract::dispatch::FixedClock as DispatchFixedClock;
    let _ = DispatchFixedClock::epoch();
}

#[test]
fn try_new_returns_err_on_zero_dim() {
    assert!(HashEmbedder::try_new(0).is_err());
}

#[test]
fn try_new_succeeds_for_positive_dim() {
    let e = HashEmbedder::try_new(384).expect("positive dim must succeed");
    assert_eq!(e.dim(), 384);
}
