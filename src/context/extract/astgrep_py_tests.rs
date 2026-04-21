use super::astgrep_py::*;
use chrono::{DateTime, Utc};

fn when() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap()
}

#[test]
fn extracts_public_function_with_docstring() {
    let src = "\
def verify_token(token: str) -> bool:
    \"\"\"Validate the JWT against signing key.\"\"\"
    return True
";
    let chunks = extract_python(src, "src/auth.py", "mini-py", "abc", when()).unwrap();
    let vt = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("verify_token"))
        .unwrap();
    assert_eq!(vt.kind, super::super::types::ChunkKind::Symbol);
    assert!(vt.content.contains("Validate the JWT"));
    assert!(vt
        .signature
        .as_ref()
        .unwrap()
        .contains("def verify_token"));
    assert!(vt.metadata.is_exported);
    assert_eq!(vt.metadata.language.as_deref(), Some("python"));
    assert_eq!(vt.provenance.extractor, "ast-grep-python");
}

#[test]
fn extracts_public_class_with_docstring() {
    let src = "\
class AuthError(Exception):
    \"\"\"Raised when auth fails.\"\"\"
";
    let chunks = extract_python(src, "src/auth.py", "mini-py", "abc", when()).unwrap();
    let ae = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("AuthError"))
        .unwrap();
    assert!(ae.content.contains("Raised when"));
    assert!(ae
        .signature
        .as_ref()
        .unwrap()
        .contains("class AuthError(Exception)"));
}

#[test]
fn skips_underscore_prefixed_without_all() {
    let src = "\
def _private(): pass
def public(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"public"));
    assert!(
        !names.contains(&"_private"),
        "underscore-prefixed names must not be extracted when __all__ is absent, got: {names:?}"
    );
}

#[test]
fn respects_dunder_all_whitelist() {
    let src = "\
__all__ = [\"foo\"]
def foo(): pass
def bar(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert_eq!(
        names,
        vec!["foo"],
        "__all__ is authoritative — only foo should be extracted, got: {names:?}"
    );
}

#[test]
fn respects_dunder_all_tuple() {
    let src = "\
__all__ = (\"foo\",)
def foo(): pass
def bar(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert_eq!(names, vec!["foo"]);
}

#[test]
fn skips_methods_inside_class() {
    let src = "\
class Foo:
    def method(self): pass
def top_level(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"Foo"));
    assert!(names.contains(&"top_level"));
    assert!(
        !names.contains(&"method"),
        "methods inside classes must not be extracted, got: {names:?}"
    );
}

#[test]
fn skips_nested_functions() {
    let src = "\
def outer():
    def inner(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert_eq!(names, vec!["outer"]);
}

#[test]
fn signature_collapses_multiline_def() {
    let src = "\
def foo(
    x: int,
    y: int,
) -> int:
    pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let sig = chunks[0].signature.as_ref().unwrap();
    assert!(
        sig.contains("foo(x: int, y: int) -> int"),
        "signature should collapse to single line, got: {sig}"
    );
    assert!(!sig.contains('\n'), "signature must not contain newlines: {sig}");
}

#[test]
fn falls_back_to_signature_when_no_docstring() {
    let src = "def foo(): pass\n";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].content.contains("def foo"));
}

#[test]
fn dunder_all_inside_comment_is_ignored() {
    let src = "\
# __all__ = [\"fake\"]
def real(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(
        names.contains(&"real"),
        "`real` must be extracted when __all__ is only in a comment, got: {names:?}"
    );
}

#[test]
fn dunder_all_inside_docstring_is_ignored() {
    let src = "\
\"\"\"Module doc mentioning __all__ = ['ghost'].\"\"\"
def real(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(
        names.contains(&"real"),
        "`real` must be extracted when __all__ is only in a docstring, got: {names:?}"
    );
}

#[test]
fn dunder_all_nested_in_function_is_ignored() {
    let src = "\
def configure():
    __all__ = [\"inner\"]
def top(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(
        names.contains(&"top"),
        "`top` must be extracted when __all__ is only inside a function, got: {names:?}"
    );
    assert!(
        names.contains(&"configure"),
        "`configure` must be extracted; got: {names:?}"
    );
}

#[test]
fn multiple_dunder_all_last_wins() {
    let src = "\
__all__ = [\"a\"]
__all__ = [\"b\"]
def a(): pass
def b(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert_eq!(
        names,
        vec!["b"],
        "last __all__ assignment must win, got: {names:?}"
    );
}

#[test]
fn same_named_items_dedupe_disambiguated() {
    let src = "\
if True:
    def foo(): pass
else:
    def foo(): pass
";
    let chunks = extract_python(src, "x.py", "s", "c", when()).unwrap();
    let foos: Vec<_> = chunks
        .iter()
        .filter(|c| c.qualified_name.as_deref() == Some("foo"))
        .collect();
    assert_eq!(
        foos.len(),
        2,
        "both sibling `foo` defs must be preserved, got {} total chunks: {:?}",
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
