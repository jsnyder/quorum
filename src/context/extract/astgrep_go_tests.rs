use super::astgrep_go::extract_go;
use chrono::Utc;

#[test]
fn extract_go_exported_functions() {
    let src = r#"package main

func PublicFunc() {}
func privateFunc() {}
func AnotherPublic(x int) error { return nil }
"#;
    let chunks = extract_go(src, "main.go", "test", "abc123", Utc::now()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"PublicFunc"));
    assert!(names.contains(&"AnotherPublic"));
    assert!(!names.contains(&"privateFunc"));
}

#[test]
fn extract_go_exported_methods() {
    let src = r#"package main

type Server struct{}

func (s *Server) Start() error { return nil }
func (s *Server) stop() {}
"#;
    let chunks = extract_go(src, "server.go", "test", "abc123", Utc::now()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(
        names.contains(&"Server.Start"),
        "expected receiver-qualified method, got: {:?}",
        names
    );
    assert!(!names.iter().any(|n| n.ends_with("stop")));
}

#[test]
fn extract_go_exported_structs() {
    let src = r#"package main

type Config struct {
    Port int
    Host string
}

type internal struct {
    x int
}
"#;
    let chunks = extract_go(src, "types.go", "test", "abc123", Utc::now()).unwrap();
    let names: Vec<_> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(names.contains(&"Config"));
    assert!(!names.contains(&"internal"));
}

#[test]
fn extract_go_empty_file() {
    let chunks = extract_go("", "empty.go", "test", "abc123", Utc::now()).unwrap();
    assert!(chunks.is_empty());
}
