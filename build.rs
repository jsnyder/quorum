fn main() {
    // Compile the tree-sitter-dockerfile grammar from vendored C sources.
    // We vendor this instead of using the tree-sitter-dockerfile crate because
    // that crate depends on tree-sitter 0.20, which conflicts with our 0.25.
    let src_dir = std::path::Path::new("grammars/tree-sitter-dockerfile/src");

    cc::Build::new()
        .include(src_dir)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .file(src_dir.join("parser.c"))
        .file(src_dir.join("scanner.c"))
        .compile("tree_sitter_dockerfile");

    println!("cargo:rerun-if-changed=grammars/tree-sitter-dockerfile/src/parser.c");
    println!("cargo:rerun-if-changed=grammars/tree-sitter-dockerfile/src/scanner.c");
}
