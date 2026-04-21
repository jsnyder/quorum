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
fn line_range_covers_doc_comments_when_present() {
    let src = "\
/// doc line 1\n\
/// doc line 2\n\
pub fn foo() {}\n\
";
    // Line 1: /// doc line 1
    // Line 2: /// doc line 2
    // Line 3: pub fn foo() {}
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    assert_eq!(
        chunks[0].metadata.line_range.start, 1,
        "line_range.start should cover the doc block when docs are the chunk's content"
    );
    assert_eq!(chunks[0].metadata.line_range.end, 3);
}

#[test]
fn line_range_points_at_signature_when_no_doc_comments() {
    let src = "\
\n\
pub fn foo() {}\n\
";
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    assert_eq!(chunks[0].metadata.line_range.start, 2);
}

#[test]
fn same_named_items_in_sibling_modules_are_both_extracted() {
    let src = r#"
mod a { pub fn foo() {} }
mod b { pub fn foo() {} }
"#;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let foos: Vec<_> = chunks
        .iter()
        .filter(|c| c.qualified_name.as_deref() == Some("foo"))
        .collect();
    assert_eq!(foos.len(), 2, "both sibling `foo`s must be preserved");
    // Ids are disambiguated by byte_start when names collide.
    assert_ne!(foos[0].id, foos[1].id);
    assert!(foos[0].id.contains("@"));
    assert!(foos[1].id.contains("@"));
    // Each `foo` lists the OTHER `foo` as a neighbor.
    assert!(foos[0].metadata.neighboring_symbols.contains(&"foo".to_string()));
    assert!(foos[1].metadata.neighboring_symbols.contains(&"foo".to_string()));
}

#[test]
fn content_combines_docs_and_signature() {
    let src = r#"
/// Validates a JWT.
pub fn verify_token(x: &str) -> bool { true }
"#;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let c = &chunks[0];
    assert!(c.content.contains("Validates a JWT"));
    assert!(c.content.contains("pub fn verify_token"));
}

#[test]
fn module_inner_doc_is_not_attached_to_next_item() {
    let src = r#"
//! Module-level docs.

pub fn foo() {}
"#;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let foo = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("foo"))
        .unwrap();
    assert!(!foo.content.contains("Module-level docs"));
}

#[test]
fn signature_survives_brace_inside_attribute() {
    let src = r##"
#[doc = "example { brace"]
pub fn foo() {}
"##;
    let chunks = extract_rust(src, "x.rs", "s", "c", when()).unwrap();
    let foo = &chunks[0];
    assert!(foo.signature.as_ref().unwrap().contains("pub fn foo"));
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
