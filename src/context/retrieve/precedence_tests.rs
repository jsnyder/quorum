use chrono::{DateTime, Utc};

use crate::context::retrieve::precedence::{resolve_precedence, SourceWeights};
use crate::context::retrieve::{ScoreBreakdown, ScoredChunk};
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn scored(
    id: &str,
    source: &str,
    qname: Option<&str>,
    indexed: DateTime<Utc>,
    score: f32,
) -> ScoredChunk {
    ScoredChunk {
        chunk: Chunk {
            id: id.into(),
            source: source.into(),
            kind: ChunkKind::Symbol,
            subtype: None,
            qualified_name: qname.map(String::from),
            signature: None,
            content: "c".into(),
            metadata: ChunkMeta {
                source_path: "x.rs".into(),
                line_range: LineRange::new(1, 1).unwrap(),
                commit_sha: "sha".into(),
                indexed_at: indexed,
                source_version: None,
                language: Some("rust".into()),
                is_exported: true,
                neighboring_symbols: vec![],
            },
            provenance: Provenance::new("t", 0.9, "x.rs").unwrap(),
        },
        score,
        components: ScoreBreakdown {
            bm25_norm: 0.0,
            vec_norm: 0.0,
            id_boost: 0.0,
            path_boost: 0.0,
            recency_mul: 1.0,
            score,
        },
        source_legs: vec![],
    }
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn no_duplicates_returns_input_unchanged() {
    let weights = SourceWeights::default();
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", Some("foo"), ts, 0.9),
        scored("2", "A", Some("bar"), ts, 0.8),
        scored("3", "A", Some("baz"), ts, 0.7),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 3);
    assert!(log.is_empty());
}

#[test]
fn chunks_without_qualified_name_always_kept() {
    let weights = SourceWeights::default();
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", None, ts, 0.9),
        scored("2", "A", Some("foo"), ts, 0.8),
        scored("3", "B", Some("foo"), ts, 0.7),
        scored("4", "A", None, ts, 0.6),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 3);
    assert_eq!(log.entries().len(), 1);
    // Both None-qname chunks are present.
    assert_eq!(
        kept.iter()
            .filter(|c| c.chunk.qualified_name.is_none())
            .count(),
        2
    );
}

#[test]
fn weight_breaks_tie() {
    let weights = SourceWeights::new([("A".into(), 10), ("B".into(), 5)]);
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", Some("foo"), ts, 0.5),
        scored("2", "B", Some("foo"), ts, 0.9),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].chunk.source, "A");
    assert_eq!(log.entries().len(), 1);
    let entry = &log.entries()[0];
    assert_eq!(entry.winner_source, "A");
    assert_eq!(entry.loser_source, "B");
    assert!(entry.reason.contains("weight"));
}

#[test]
fn indexed_at_breaks_tie_when_weights_equal() {
    let weights = SourceWeights::default();
    let input = vec![
        scored("1", "A", Some("foo"), t("2024-01-01T00:00:00Z"), 0.5),
        scored("2", "B", Some("foo"), t("2025-01-01T00:00:00Z"), 0.5),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].chunk.source, "B");
    assert_eq!(log.entries().len(), 1);
    assert!(log.entries()[0].reason.contains("indexed"));
}

#[test]
fn alphabetical_breaks_tie_when_weights_and_indexed_at_equal() {
    let weights = SourceWeights::default();
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "B", Some("foo"), ts, 0.5),
        scored("2", "A", Some("foo"), ts, 0.5),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].chunk.source, "A");
    assert_eq!(log.entries().len(), 1);
    assert!(log.entries()[0].reason.contains("alphabetical"));
}

#[test]
fn three_way_competition_picks_one_winner() {
    let weights = SourceWeights::new([
        ("A".into(), 10),
        ("B".into(), 5),
        ("C".into(), 1),
    ]);
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", Some("foo"), ts, 0.5),
        scored("2", "B", Some("foo"), ts, 0.5),
        scored("3", "C", Some("foo"), ts, 0.5),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].chunk.source, "A");
    assert_eq!(log.entries().len(), 2);
    for e in log.entries() {
        assert_eq!(e.winner_source, "A");
        assert!(e.loser_source == "B" || e.loser_source == "C");
    }
}

#[test]
fn preserves_input_order_of_kept() {
    let weights = SourceWeights::new([("A".into(), 1), ("B".into(), 10)]);
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("X", "S1", None, ts, 0.9),
        scored("A1", "A", Some("foo"), ts, 0.5), // loser
        scored("Y", "S2", None, ts, 0.8),
        scored("B1", "B", Some("foo"), ts, 0.5), // winner
    ];
    let (kept, _log) = resolve_precedence(input, &weights);
    let ids: Vec<&str> = kept.iter().map(|c| c.chunk.id.as_str()).collect();
    assert_eq!(ids, vec!["X", "Y", "B1"]);
}

#[test]
fn source_not_in_weights_defaults_to_zero() {
    let weights = SourceWeights::default();
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", Some("foo"), ts, 0.5),
        scored("2", "B", Some("foo"), ts, 0.5),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].chunk.source, "A");
    assert_eq!(log.entries().len(), 1);
}

#[test]
fn log_reason_includes_weight_delta_when_applicable() {
    let weights = SourceWeights::new([("A".into(), 10), ("B".into(), 5)]);
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", Some("foo"), ts, 0.5),
        scored("2", "B", Some("foo"), ts, 0.5),
    ];
    let (_kept, log) = resolve_precedence(input, &weights);
    let reason = &log.entries()[0].reason;
    assert!(reason.contains("weight"));
    assert!(reason.contains("10"));
    assert!(reason.contains('5'));
}

#[test]
fn empty_input_returns_empty_kept_and_empty_log() {
    let weights = SourceWeights::default();
    let (kept, log) = resolve_precedence(vec![], &weights);
    assert!(kept.is_empty());
    assert!(log.is_empty());
}

#[test]
fn duplicate_qname_across_same_source_is_a_no_conflict() {
    // Two chunks, same source "A", same qname "foo". Must pick one winner
    // deterministically without crashing.
    let weights = SourceWeights::default();
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("chunk_b", "A", Some("foo"), ts, 0.5),
        scored("chunk_a", "A", Some("foo"), ts, 0.5),
    ];
    let (kept, log) = resolve_precedence(input, &weights);
    assert_eq!(kept.len(), 1);
    assert_eq!(log.entries().len(), 1);
    // chunk.id asc fallback: "chunk_a" < "chunk_b".
    assert_eq!(kept[0].chunk.id, "chunk_a");
}

#[test]
fn precedence_log_entries_ordered_by_qualified_name() {
    let weights = SourceWeights::new([("A".into(), 10), ("B".into(), 1)]);
    let ts = t("2025-01-01T00:00:00Z");
    let input = vec![
        scored("1", "A", Some("zeta"), ts, 0.9),
        scored("2", "B", Some("zeta"), ts, 0.9),
        scored("3", "A", Some("alpha"), ts, 0.9),
        scored("4", "B", Some("alpha"), ts, 0.9),
        scored("5", "A", Some("mu"), ts, 0.9),
        scored("6", "B", Some("mu"), ts, 0.9),
    ];
    let (_, log) = resolve_precedence(input, &weights);
    let names: Vec<_> = log.entries().iter().map(|e| e.qualified_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha", "mu", "zeta"],
        "precedence log must iterate qualified_names in sorted order"
    );
}

#[test]
fn indexed_at_reason_distinguishes_same_day_timestamps() {
    let weights = SourceWeights::default();
    let earlier = t("2026-04-21T08:00:00Z");
    let later = t("2026-04-21T18:30:00Z");
    let input = vec![
        scored("1", "A", Some("f"), earlier, 0.9),
        scored("2", "A", Some("f"), later, 0.9),
    ];
    let (_, log) = resolve_precedence(input, &weights);
    let reason = &log.entries()[0].reason;
    assert!(reason.starts_with("indexed "), "reason: {reason}");
    assert!(
        reason.contains("T18:30:00Z") && reason.contains("T08:00:00Z"),
        "reason must include time-of-day: {reason}"
    );
}
