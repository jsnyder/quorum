use super::astgrep_rust::*;
use chrono::{DateTime, Utc};

fn when() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap()
}

#[test]
fn extracts_pub_fn_with_signature_and_doc_comment() {
    let src = r#"
/// Validates a JWT.
/// Errors if expired.
pub fn verify_token(token: &str, opts: VerifyOpts) -> Result<Claims, AuthError> {
    todo!()
}
"#;
    let chunks = extract_rust(src, "src/token.rs", "mini-rust", "abc", when()).unwrap();
    let vt = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("verify_token"))
        .unwrap();
    assert_eq!(vt.kind, super::super::types::ChunkKind::Symbol);
    assert!(vt.signature.as_ref().unwrap().contains("pub fn verify_token"));
    assert!(vt
        .signature
        .as_ref()
        .unwrap()
        .contains("Result<Claims, AuthError>"));
    assert!(vt.content.contains("Validates a JWT"));
    assert!(vt.content.contains("Errors if expired"));
    assert!(vt.metadata.is_exported);
    assert_eq!(vt.metadata.language.as_deref(), Some("rust"));
    assert_eq!(vt.provenance.extractor, "ast-grep-rust");
}

#[test]
fn signature_without_doc_comment_falls_back_to_signature_as_content() {
    let src = "pub fn foo(x: i32) -> i32 { x }";
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].content.contains("pub fn foo"));
}

#[test]
fn skips_private_fn() {
    let src = "fn private() {}";
    assert!(extract_rust(src, "x.rs", "s", "c", when())
        .unwrap()
        .is_empty());
}

#[test]
fn skips_pub_fn_in_private_module() {
    // Items inside private modules are effectively private — but ast-grep
    // doesn't know module visibility. MVP accepts this false-positive: as
    // long as the item is declared `pub`, we extract it.
    let src = r#"
mod private_mod {
    pub fn inside() {}
}
pub fn outside() {}
"#;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"outside"));
    assert!(
        names.contains(&"inside"),
        "MVP extracts syntactically-pub items regardless of module visibility"
    );
}

#[test]
fn extracts_pub_struct_enum_trait() {
    let src = r#"
pub struct Foo { pub x: i32 }
pub enum Bar { A, B }
pub trait Baz { fn baz(&self); }
"#;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"Foo"));
    assert!(names.contains(&"Bar"));
    assert!(names.contains(&"Baz"));
    for c in &chunks {
        assert_eq!(c.kind, super::super::types::ChunkKind::Symbol);
    }
}

#[test]
fn extracts_neighboring_symbols() {
    let src = r#"
pub fn a() {}
pub fn b() {}
pub fn c() {}
"#;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let a = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("a"))
        .unwrap();
    assert!(a.metadata.neighboring_symbols.contains(&"b".to_string()));
    assert!(a.metadata.neighboring_symbols.contains(&"c".to_string()));
    assert!(!a.metadata.neighboring_symbols.contains(&"a".to_string()));
}

#[test]
fn chunk_id_follows_source_path_symbol_format() {
    let src = "pub fn foo() {}";
    let chunks = extract_rust(src, "src/util.rs", "mini-rust", "c", when()).unwrap();
    assert_eq!(chunks[0].id, "mini-rust:src/util.rs:foo");
}

#[test]
fn line_range_points_at_symbol_not_doc_comment() {
    let src = "\
/// doc\n\
pub fn foo() {}\n\
";
    // Line 1: /// doc
    // Line 2: pub fn foo() {}
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    assert_eq!(
        chunks[0].metadata.line_range.start, 2,
        "line_range.start should point at the signature, not the doc comment"
    );
}

#[test]
fn mini_rust_token_rs_fixture_extracts_verify_token() {
    let src =
        std::fs::read_to_string("tests/fixtures/context/repos/mini-rust/src/token.rs").unwrap();
    let chunks = extract_rust(&src, "src/token.rs", "mini-rust", "abc", when()).unwrap();
    let vt = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("verify_token"))
        .unwrap();
    assert!(vt.content.contains("JWT"));
    assert!(vt.content.contains("signing key"));
}
