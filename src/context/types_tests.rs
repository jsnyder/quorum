use super::types::*;
use chrono::{DateTime, Utc};

fn test_chunk() -> Chunk {
    Chunk {
        id: "mini-rust:src/token.rs:verify_token".into(),
        source: "mini-rust".into(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: Some("token::verify_token".into()),
        signature: Some(
            "pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError>"
                .into(),
        ),
        content: "Validates a JWT against the signing key.".into(),
        metadata: ChunkMeta {
            source_path: "src/token.rs".to_string(),
            line_range: LineRange::new(10, 25).unwrap(),
            commit_sha: "abc123".into(),
            indexed_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            source_version: None,
            language: Some("rust".into()),
            is_exported: true,
            neighboring_symbols: vec!["sign_jwt".into(), "decode_jwt".into()],
        },
        provenance: Provenance {
            extractor: "ast-grep-rust".into(),
            confidence: 1.0,
            source_uri: "git://mini-rust@abc123/src/token.rs#L10-25".into(),
        },
    }
}

#[test]
fn chunk_serializes_to_single_jsonl_line_and_roundtrips() {
    let chunk = test_chunk();
    let line = serde_json::to_string(&chunk).unwrap();
    assert!(
        !line.contains('\n'),
        "JSONL lines must be single-line: {line}"
    );
    let decoded: Chunk = serde_json::from_str(&line).unwrap();
    assert_eq!(decoded, chunk);
}

#[test]
fn chunk_kind_serializes_as_snake_case() {
    assert_eq!(
        serde_json::to_string(&ChunkKind::Symbol).unwrap(),
        "\"symbol\""
    );
    assert_eq!(serde_json::to_string(&ChunkKind::Doc).unwrap(), "\"doc\"");
    assert_eq!(
        serde_json::to_string(&ChunkKind::Schema).unwrap(),
        "\"schema\""
    );

    // Deserialization too
    let k: ChunkKind = serde_json::from_str("\"symbol\"").unwrap();
    assert_eq!(k, ChunkKind::Symbol);
}

#[test]
fn none_option_fields_are_omitted_from_json() {
    let mut chunk = test_chunk();
    chunk.qualified_name = None;
    chunk.signature = None;
    chunk.subtype = None;
    chunk.metadata.source_version = None;
    chunk.metadata.language = None;
    let json = serde_json::to_string(&chunk).unwrap();
    assert!(
        !json.contains("\"qualified_name\""),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("\"signature\""),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("\"subtype\""),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("\"source_version\""),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("\"language\""),
        "None fields should be omitted: {json}"
    );
}

#[test]
fn missing_option_fields_parse_as_none() {
    // Minimal JSON missing all optional fields
    let json = r#"{
        "id": "s:p:q",
        "source": "s",
        "kind": "doc",
        "content": "hello",
        "metadata": {
            "source_path": "p",
            "line_range": {"start": 1, "end": 2},
            "commit_sha": "c",
            "indexed_at": "2026-01-01T00:00:00Z",
            "is_exported": true,
            "neighboring_symbols": []
        },
        "provenance": {
            "extractor": "e",
            "confidence": 1.0,
            "source_uri": "u"
        }
    }"#;
    let chunk: Chunk = serde_json::from_str(json).unwrap();
    assert!(chunk.qualified_name.is_none());
    assert!(chunk.signature.is_none());
    assert!(chunk.subtype.is_none());
    assert!(chunk.metadata.language.is_none());
}

#[test]
fn unknown_fields_parse_cleanly_for_forward_compat() {
    // If a future version of quorum writes extra fields, older binaries
    // must still parse the JSON without error.
    let json = r#"{
        "id": "s:p:q",
        "source": "s",
        "kind": "symbol",
        "content": "hello",
        "metadata": {
            "source_path": "p",
            "line_range": {"start": 1, "end": 2},
            "commit_sha": "c",
            "indexed_at": "2026-01-01T00:00:00Z",
            "is_exported": true,
            "neighboring_symbols": [],
            "future_field": {"added_in": "v3"}
        },
        "provenance": {
            "extractor": "e",
            "confidence": 1.0,
            "source_uri": "u"
        },
        "another_future_field": 42
    }"#;
    let chunk: Chunk = serde_json::from_str(json).unwrap();
    assert_eq!(chunk.id, "s:p:q");
}

#[test]
fn line_range_rejects_zero_start() {
    let json = r#"{"start": 0, "end": 5}"#;
    let err = serde_json::from_str::<LineRange>(json).unwrap_err();
    assert!(err.to_string().contains("1-indexed"), "got: {err}");
}

#[test]
fn line_range_new_rejects_zero_start() {
    let err = LineRange::new(0, 5).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("start"), "got: {msg}");
}

#[test]
fn line_range_new_rejects_inverted() {
    let err = LineRange::new(10, 5).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("must be >= start"), "got: {msg}");
}

#[test]
fn line_range_new_accepts_valid() {
    let r = LineRange::new(1, 1).expect("1..=1 is valid");
    assert_eq!(r.start(), 1);
    assert_eq!(r.end(), 1);

    let r = LineRange::new(3, 10).expect("3..=10 is valid");
    assert_eq!(r.start(), 3);
    assert_eq!(r.end(), 10);
}

#[test]
fn provenance_new_rejects_nan() {
    let err = Provenance::new("x", f32::NAN, "u").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("finite"), "got: {msg}");
}

#[test]
fn provenance_new_rejects_out_of_range() {
    let err = Provenance::new("x", 1.5, "u").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("[0.0, 1.0]"), "got: {msg}");
}

#[test]
fn provenance_new_rejects_negative() {
    let err = Provenance::new("x", -0.1, "u").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("[0.0, 1.0]"), "got: {msg}");
}

#[test]
fn provenance_new_accepts_valid() {
    for c in [0.0_f32, 0.5, 1.0] {
        let p = Provenance::new("x", c, "u").expect("valid confidence");
        assert_eq!(p.extractor(), "x");
        assert_eq!(p.confidence(), c);
        assert_eq!(p.source_uri(), "u");
    }
}

#[test]
fn line_range_rejects_inverted_range() {
    let json = r#"{"start": 10, "end": 5}"#;
    let err = serde_json::from_str::<LineRange>(json).unwrap_err();
    assert!(err.to_string().contains("<= end"), "got: {err}");
}

#[test]
fn provenance_rejects_nan_confidence() {
    let _json = r#"{"extractor": "x", "confidence": "NaN", "source_uri": "u"}"#;
    // NaN can't be expressed in JSON directly; we need to test via f32 path
    let json_inf = r#"{"extractor": "x", "confidence": 1.0e300, "source_uri": "u"}"#;
    // 1.0e300 as f32 is infinity
    let err = serde_json::from_str::<Provenance>(json_inf).unwrap_err();
    assert!(
        err.to_string().contains("finite") || err.to_string().contains("[0.0, 1.0]"),
        "got: {err}"
    );
}

#[test]
fn provenance_rejects_out_of_range_confidence() {
    let json_high = r#"{"extractor": "x", "confidence": 1.5, "source_uri": "u"}"#;
    let err = serde_json::from_str::<Provenance>(json_high).unwrap_err();
    assert!(err.to_string().contains("[0.0, 1.0]"), "got: {err}");

    let json_neg = r#"{"extractor": "x", "confidence": -0.1, "source_uri": "u"}"#;
    let err = serde_json::from_str::<Provenance>(json_neg).unwrap_err();
    assert!(err.to_string().contains("[0.0, 1.0]"), "got: {err}");
}
