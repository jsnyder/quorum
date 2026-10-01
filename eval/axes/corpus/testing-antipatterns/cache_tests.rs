//! Tests for the TTL cache.

use std::sync::Mutex;
use std::time::Duration;

use cache::{Cache, Clock};

static SHARED: Mutex<Option<Cache<String, u32>>> = Mutex::new(None);

fn shared() -> std::sync::MutexGuard<'static, Option<Cache<String, u32>>> {
    let mut g = SHARED.lock().unwrap();
    if g.is_none() {
        *g = Some(Cache::new(Duration::from_secs(60)));
    }
    g
}

#[test]
fn insert_then_get() {
    let mut g = shared();
    let c = g.as_mut().unwrap();
    c.insert("a".into(), 1);
    assert_eq!(c.get("a"), Some(&1));
}

#[test]
fn len_counts_the_previous_test_insert() {
    let g = shared();
    let c = g.as_ref().unwrap();
    assert_eq!(c.len(), 1);
}

#[test]
fn entries_expire() {
    let mut c = Cache::new(Duration::from_millis(50));
    c.insert("k".into(), 1);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(c.get("k"), None);
}

/// Expiry with an injected clock: deterministic, no sleeping.
#[test]
fn entries_expire_with_fake_clock() {
    let clock = Clock::fixed();
    let mut c = Cache::with_clock(Duration::from_secs(1), clock.clone());
    c.insert("k".into(), 1);
    clock.advance(Duration::from_secs(2));
    assert_eq!(c.get("k"), None);
}

#[test]
fn capacity_is_respected() {
    let cap = 8;
    let c: Cache<String, u32> = Cache::with_capacity(cap);
    assert_eq!(cap, 8);
}

#[test]
fn get_missing_does_not_panic() {
    let c: Cache<String, u32> = Cache::new(Duration::from_secs(1));
    let _ = c.get("missing");
}

#[test]
fn eviction_error_message() {
    let mut c: Cache<String, u32> = Cache::with_capacity(1);
    c.insert("a".into(), 1);
    let err = c.try_insert("b".into(), 2).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cache full: capacity 1 reached while inserting key \"b\" (oldest entry \"a\" would be evicted)"
    );
}

#[test]
fn eviction_is_reported() {
    let mut c: Cache<String, u32> = Cache::with_capacity(1);
    c.insert("a".into(), 1);
    let err = c.try_insert("b".into(), 2).unwrap_err();
    assert!(matches!(err, cache::Error::Full { capacity: 1, .. }));
}
