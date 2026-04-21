use super::astgrep_ts::*;
use chrono::{DateTime, Utc};

fn when() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap()
}

#[test]
fn extracts_exported_function_with_jsdoc() {
    let src = "\
/**
 * Validates a JWT against the service's signing key.
 * Returns AuthError when token.exp is in the past.
 */
export function verifyToken(token: string, opts: VerifyOpts): Claims | AuthError {
    throw new Error(\"unimplemented\");
}
";
    let chunks = extract_typescript(src, "src/auth.ts", "mini-ts", "abc", when()).unwrap();
    let vt = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("verifyToken"))
        .unwrap();
    assert_eq!(vt.kind, super::super::types::ChunkKind::Symbol);
    assert!(vt
        .signature
        .as_ref()
        .unwrap()
        .contains("export function verifyToken"));
    assert!(vt
        .signature
        .as_ref()
        .unwrap()
        .contains("Claims | AuthError"));
    assert!(vt.content.contains("Validates a JWT"));
    assert!(vt.content.contains("signing key"));
    assert!(vt.metadata.is_exported);
    assert_eq!(vt.metadata.language.as_deref(), Some("typescript"));
    assert_eq!(vt.provenance.extractor, "ast-grep-typescript");
}

#[test]
fn extracts_exported_interface() {
    let src = "export interface VerifyOpts { allowExpired: boolean }\n";
    let chunks = extract_typescript(src, "src/auth.ts", "mini-ts", "c", when()).unwrap();
    let iface = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("VerifyOpts"))
        .expect("VerifyOpts not extracted");
    assert_eq!(iface.kind, super::super::types::ChunkKind::Symbol);
    assert!(iface
        .signature
        .as_ref()
        .unwrap()
        .contains("export interface VerifyOpts"));
}

#[test]
fn extracts_exported_class() {
    let src = "export class Foo { bar(): number { return 1; } }\n";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let c = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("Foo"))
        .expect("Foo not extracted");
    assert_eq!(c.kind, super::super::types::ChunkKind::Symbol);
    assert!(c.signature.as_ref().unwrap().contains("export class Foo"));
}

#[test]
fn extracts_exported_type_alias() {
    let src = "export type Foo = string | number;\n";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let t = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("Foo"))
        .expect("Foo type alias not extracted");
    assert_eq!(t.kind, super::super::types::ChunkKind::Symbol);
    assert!(t.signature.as_ref().unwrap().contains("export type Foo"));
}

#[test]
fn skips_re_exports() {
    let src = "export { verifyToken } from \"./auth\";\n";
    let chunks = extract_typescript(src, "src/index.ts", "mini-ts", "c", when()).unwrap();
    assert!(
        chunks.is_empty(),
        "re-exports must not produce symbol chunks, got {chunks:?}"
    );
}

#[test]
fn skips_non_exported_items() {
    let src = "\
function privateFn() {}
class PrivateClass {}
interface PrivateIface { x: number }
type PrivateType = string;
";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    assert!(
        chunks.is_empty(),
        "non-exported items must not be extracted, got {chunks:?}"
    );
}

#[test]
fn signature_strips_function_body() {
    let src = "\
export function foo(x: number): number {
    return x + 1;
}
";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let sig = chunks[0].signature.as_ref().unwrap();
    assert!(!sig.contains("return"));
    assert!(!sig.contains('{'));
    assert!(sig.contains("export function foo(x: number): number"));
}

#[test]
fn jsdoc_contiguity() {
    // Blank line between JSDoc and symbol breaks the association.
    let src = "\
/** not attached */

export function foo() {}
";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let foo = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("foo"))
        .unwrap();
    assert!(
        !foo.content.contains("not attached"),
        "JSDoc separated by blank line must not be associated, got content: {}",
        foo.content
    );
}

#[test]
fn same_named_exports_dedupe_disambiguated() {
    // Two sibling namespaces each export `foo`. Both chunks survive, with
    // distinct ids disambiguated by byte_start.
    let src = "\
export namespace a { export function foo() {} }
export namespace b { export function foo() {} }
";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let foos: Vec<_> = chunks
        .iter()
        .filter(|c| c.qualified_name.as_deref() == Some("foo"))
        .collect();
    assert_eq!(
        foos.len(),
        2,
        "both sibling `foo` exports must be preserved, got {} chunks total: {:?}",
        foos.len(),
        chunks
            .iter()
            .map(|c| c.qualified_name.as_deref().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
    assert_ne!(foos[0].id, foos[1].id);
    assert!(foos[0].id.contains('@'));
    assert!(foos[1].id.contains('@'));
}

#[test]
fn line_range_covers_jsdoc_when_present() {
    let src = "\
/**
 * docs
 */
export function foo() {}
";
    // Line 1: /**
    // Line 2:  * docs
    // Line 3:  */
    // Line 4: export function foo() {}
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let foo = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("foo"))
        .unwrap();
    assert_eq!(
        foo.metadata.line_range.start, 1,
        "line_range.start should equal the JSDoc's opening line"
    );
    assert_eq!(foo.metadata.line_range.end, 4);
}

#[test]
fn signature_preserves_rhs_for_type_alias() {
    // `{` on the RHS of a type alias is NOT a body opener — it must be kept.
    let src = "export type Point = { x: number; y: number };\n";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let t = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("Point"))
        .expect("Point type alias not extracted");
    let sig = t.signature.as_ref().unwrap();
    assert!(
        sig.contains("x: number"),
        "type alias RHS must be preserved, got: {sig}"
    );
    assert!(
        sig.contains("y: number"),
        "type alias RHS must be preserved, got: {sig}"
    );
    assert!(sig.contains("export type Point ="), "got: {sig}");
}

#[test]
fn signature_strips_body_for_class() {
    // Class bodies must still be stripped (regression guard for the kind-aware
    // signature logic).
    let src = "\
export class Widget {
    private id: number = 0;
    render(): void { return; }
}
";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    let w = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("Widget"))
        .expect("Widget class not extracted");
    let sig = w.signature.as_ref().unwrap();
    assert!(sig.contains("export class Widget"), "got: {sig}");
    assert!(!sig.contains('{'), "class body must be stripped, got: {sig}");
    assert!(!sig.contains("render"), "class body must be stripped, got: {sig}");
    assert!(!sig.contains("private id"), "class body must be stripped, got: {sig}");
}

#[test]
fn mini_ts_auth_fixture_extracts_verify_token() {
    let src =
        std::fs::read_to_string("tests/fixtures/context/repos/mini-ts/src/auth.ts").unwrap();
    let chunks = extract_typescript(&src, "src/auth.ts", "mini-ts", "abc", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"VerifyOpts"));
    assert!(names.contains(&"Claims"));
    assert!(names.contains(&"AuthError"));
    assert!(names.contains(&"verifyToken"));

    let vt = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("verifyToken"))
        .unwrap();
    assert!(vt.content.contains("JWT"));
    assert!(vt.content.contains("signing key"));
}

#[test]
fn type_alias_with_semicolon_in_string() {
    let src = r#"export type Delim = "a;b";"#;
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    assert_eq!(chunks.len(), 1);
    let sig = chunks[0].signature.as_ref().unwrap();
    assert!(sig.contains("\"a;b\""), "signature was: {sig}");
    assert!(sig.contains("export type Delim"), "signature was: {sig}");
}

#[test]
fn type_alias_with_semicolon_in_template_literal() {
    let src = "export type T = `a;b`;\n";
    let chunks = extract_typescript(src, "x.ts", "s", "c", when()).unwrap();
    assert_eq!(chunks.len(), 1);
    let sig = chunks[0].signature.as_ref().unwrap();
    assert!(sig.contains("`a;b`"), "signature was: {sig}");
}
