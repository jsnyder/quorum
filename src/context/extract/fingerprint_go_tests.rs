use super::fingerprint_go::GoFingerprinter;

#[test]
fn fingerprint_go_simple_function() {
    let src = r#"package main

import "fmt"

func processItems(items []string) error {
    for _, item := range items {
        if item == "" {
            continue
        }
        fmt.Println(item)
    }
    return nil
}
"#;
    let fp = GoFingerprinter.fingerprint_source(src);
    assert!(
        fp.is_some(),
        "non-trivial Go function should produce a fingerprint"
    );
    let fp = fp.unwrap();
    assert!(fp.control_flow.loops >= 1);
    assert!(fp.control_flow.branches >= 1);
}

#[test]
fn fingerprint_go_trivial_function_skipped() {
    let src = "package main\n\nfunc trivial() int { return 42 }\n";
    let fp = GoFingerprinter.fingerprint_source(src);
    assert!(fp.is_none(), "trivial function should be skipped");
}

#[test]
fn fingerprint_go_method() {
    let src = r#"package main

type Server struct{ port int }

func (s *Server) Start() error {
    if s.port == 0 {
        s.port = 8080
    }
    listener, err := net.Listen("tcp", fmt.Sprintf(":%d", s.port))
    if err != nil {
        return err
    }
    defer listener.Close()
    for {
        conn, err := listener.Accept()
        if err != nil {
            return err
        }
        go s.handleConn(conn)
    }
}
"#;
    let results = GoFingerprinter.fingerprint_all_functions(src);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "Server.Start");
    assert!(results[0].1.signature.is_method);
}

#[test]
fn fingerprint_go_multiple_functions() {
    let src = r#"package main

import "fmt"

func first(a int, b int) int {
    result := 0
    for i := 0; i < a; i++ {
        if i%b == 0 {
            result += i
            fmt.Println(result)
        }
    }
    return result
}

func second(items []string) {
    for _, item := range items {
        if item != "" {
            fmt.Println(item)
            fmt.Println(len(item))
        }
    }
}
"#;
    let results = GoFingerprinter.fingerprint_all_functions(src);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "first");
    assert_eq!(results[1].0, "second");
}
