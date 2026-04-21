use super::types::*;
use chrono::{DateTime, Utc};
use std::path::PathBuf;

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
            source_path: PathBuf::from("src/token.rs"),
            line_range: (10, 25),
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
        !json.contains("qualified_name"),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("signature"),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("subtype"),
        "None fields should be omitted: {json}"
    );
    assert!(
        !json.contains("source_version"),
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
            "line_range": [1, 2],
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
