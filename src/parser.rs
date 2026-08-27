use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Tsx,
    Yaml,
    Bash,
    Dockerfile,
    Terraform,
    Go,
    /// C and C++ share one variant. tree-sitter-cpp parses C as a subset, and
    /// `.h` is ambiguous between the two -- splitting would force a guess on
    /// the most common extension of all. Split it when a rule needs to fire on
    /// one and not the other.
    Cpp,
}

impl Language {
    /// Every variant, in catalog order. The MCP catalog and `from_extension`
    /// are both derived from this, so a new language cannot be added without
    /// being advertised and routable (#483).
    pub const ALL: &'static [Language] = &[
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::Tsx,
        Language::Yaml,
        Language::Bash,
        Language::Dockerfile,
        Language::Terraform,
        Language::Go,
        Language::Cpp,
    ];

    /// Stable lowercase slug. Used for LLM prompt language tags, MCP responses
    /// and the catalog -- previously duplicated across four match statements.
    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Yaml => "yaml",
            Language::Bash => "bash",
            Language::Dockerfile => "dockerfile",
            Language::Terraform => "terraform",
            Language::Go => "go",
            Language::Cpp => "cpp",
        }
    }

    /// Extensions this language claims, lowercase and without the leading dot.
    /// The single source of truth for extension routing.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::Python => &["py"],
            Language::TypeScript => &["ts", "js", "mjs", "cjs"],
            Language::Tsx => &["tsx", "jsx"],
            Language::Yaml => &["yaml", "yml"],
            Language::Bash => &["sh", "bash", "zsh", "bats"],
            Language::Dockerfile => &["dockerfile"],
            Language::Terraform => &["tf", "tfvars"],
            Language::Go => &["go"],
            Language::Cpp => &["c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx"],
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_ascii_lowercase();
        Language::ALL
            .iter()
            .find(|lang| lang.extensions().contains(&ext.as_str()))
            .copied()
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        // Check filename first for extensionless files (Dockerfile, Dockerfile.prod, etc.)
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.to_lowercase().starts_with("dockerfile")
        {
            return Some(Language::Dockerfile);
        }
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            Language::Dockerfile => {
                // Grammar is vendored and compiled via build.rs to avoid
                // linking tree-sitter 0.20 (which the crate depends on).
                unsafe extern "C" {
                    fn tree_sitter_dockerfile() -> *const ();
                }
                let lang_fn =
                    unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_dockerfile) };
                lang_fn.into()
            }
            Language::Terraform => tree_sitter_hcl::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }

    fn function_node_kinds(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["function_item"],
            Language::Python => &["function_definition"],
            Language::TypeScript | Language::Tsx => &["function_declaration", "method_definition"],
            Language::Yaml => &[],
            Language::Bash => &["function_definition"],
            Language::Dockerfile => &[],
            Language::Terraform => &[],
            Language::Go => &["function_declaration", "method_declaration"],
            // Covers free functions, methods defined out of line, and member
            // functions defined inside a class body -- tree-sitter-cpp uses
            // function_definition for all three.
            Language::Cpp => &["function_definition"],
        }
    }
}

pub fn parse(source: &str, lang: Language) -> anyhow::Result<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang.tree_sitter_language())?;
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed"))
}

pub struct FunctionInfo {
    pub name: String,
    pub line_start: u32,
    pub line_end: u32,
}

/// tree-sitter-cpp puts no `name` field on `function_definition`. The identifier
/// hangs off `declarator`, wrapped in one extra node per pointer or reference in
/// the return type (`char **f()` nests two deep), so descend until an actual
/// name kind turns up. Returns None rather than looping if the chain runs out.
fn cpp_function_name(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cur = node.child_by_field_name("declarator")?;
    loop {
        match cur.kind() {
            "identifier"
            | "field_identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name" => return Some(cur),
            // `reference_declarator` (`int &f()`) holds its inner declarator as
            // a positional child with no field name, unlike `pointer_declarator`.
            // Fall back to scanning when the field lookup comes up empty.
            _ => {
                cur = cur.child_by_field_name("declarator").or_else(|| {
                    (0..cur.named_child_count())
                        .filter_map(|i| cur.named_child(i as u32))
                        .find(|c| {
                            c.kind().ends_with("declarator") || c.kind().ends_with("identifier")
                        })
                })?
            }
        }
    }
}

pub fn extract_functions(
    tree: &tree_sitter::Tree,
    source: &str,
    lang: Language,
) -> Vec<FunctionInfo> {
    let mut functions = Vec::new();
    let kinds = lang.function_node_kinds();
    let is_ts = matches!(lang, Language::TypeScript | Language::Tsx);

    // Iterative depth-first traversal (avoids stack overflow on deep trees)
    let mut cursor = tree.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();

            // Standard named functions/methods
            if kinds.contains(&node.kind())
                && let Some(name_node) = node.child_by_field_name("name").or_else(|| {
                    matches!(lang, Language::Cpp)
                        .then(|| cpp_function_name(node))
                        .flatten()
                })
            {
                let name = &source[name_node.byte_range()];
                functions.push(FunctionInfo {
                    name: name.to_string(),
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                });
            }

            // Arrow functions: const name = (...) => { ... }
            // Tree shape: lexical_declaration > variable_declarator[name, value=arrow_function]
            if is_ts
                && node.kind() == "arrow_function"
                && let Some(parent) = node.parent()
                && parent.kind() == "variable_declarator"
                && let Some(name_node) = parent.child_by_field_name("name")
            {
                let name = &source[name_node.byte_range()];
                functions.push(FunctionInfo {
                    name: name.to_string(),
                    line_start: parent.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                });
            }
        }

        // Iterative tree walk: down, right, or up
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            did_visit = true;
            continue;
        }
        break;
    }
    functions
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Language detection --

    #[test]
    fn detect_language_rust() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
    }

    #[test]
    fn detect_language_python() {
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
    }

    #[test]
    fn detect_language_typescript() {
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
    }

    #[test]
    fn detect_language_tsx() {
        assert_eq!(Language::from_extension("tsx"), Some(Language::Tsx));
    }

    #[test]
    fn detect_language_unknown_returns_none() {
        assert_eq!(Language::from_extension("xyz"), None);
    }

    #[test]
    fn detect_language_from_path() {
        assert_eq!(
            Language::from_path(std::path::Path::new("src/main.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("app.py")),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("no_extension")),
            None
        );
    }

    // -- Parsing --

    #[test]
    fn parse_valid_rust() {
        let tree = parse("fn main() { println!(\"hello\"); }", Language::Rust).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_valid_python() {
        let tree = parse("def hello():\n    print('hi')\n", Language::Python).unwrap();
        assert_eq!(tree.root_node().kind(), "module");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_valid_typescript() {
        let tree = parse(
            "function hello(): void { console.log('hi'); }",
            Language::TypeScript,
        )
        .unwrap();
        assert_eq!(tree.root_node().kind(), "program");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_empty_file() {
        let tree = parse("", Language::Rust).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_detects_syntax_errors() {
        let tree = parse("fn {{{{{", Language::Rust).unwrap();
        assert!(tree.root_node().has_error());
    }

    // -- Function extraction --

    #[test]
    fn extract_functions_rust() {
        let source = "fn foo() {} fn bar() {} struct Baz;";
        let tree = parse(source, Language::Rust).unwrap();
        let fns = extract_functions(&tree, source, Language::Rust);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar"]);
    }

    #[test]
    fn extract_functions_python() {
        let source = "def foo():\n    pass\n\ndef bar():\n    pass\n\nclass Baz:\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let fns = extract_functions(&tree, source, Language::Python);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar"]);
    }

    #[test]
    fn extract_functions_typescript() {
        let source = "function foo() {} function bar() {} const x = 1;";
        let tree = parse(source, Language::TypeScript).unwrap();
        let fns = extract_functions(&tree, source, Language::TypeScript);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar"]);
    }

    #[test]
    fn extract_functions_preserves_line_numbers() {
        let source = "// comment\nfn foo() {}\n// gap\nfn bar() {}\n";
        let tree = parse(source, Language::Rust).unwrap();
        let fns = extract_functions(&tree, source, Language::Rust);
        assert_eq!(fns[0].name, "foo");
        assert_eq!(fns[0].line_start, 2); // 1-indexed
        assert_eq!(fns[1].name, "bar");
        assert_eq!(fns[1].line_start, 4);
    }

    #[test]
    fn extract_functions_empty_file() {
        let tree = parse("", Language::Rust).unwrap();
        let fns = extract_functions(&tree, "", Language::Rust);
        assert!(fns.is_empty());
    }

    // -- Extended function extraction (review feedback fixes) --

    #[test]
    fn extract_functions_python_async() {
        let source = "async def fetch():\n    pass\n\ndef sync():\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let fns = extract_functions(&tree, source, Language::Python);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"fetch"),
            "async functions should be extracted"
        );
        assert!(names.contains(&"sync"));
    }

    #[test]
    fn extract_functions_typescript_arrow() {
        let source = "const greet = (name: string) => { return name; };\nfunction foo() {}";
        let tree = parse(source, Language::TypeScript).unwrap();
        let fns = extract_functions(&tree, source, Language::TypeScript);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"greet"),
            "arrow functions assigned to const should be extracted"
        );
        assert!(names.contains(&"foo"));
    }

    #[test]
    fn extract_functions_typescript_method() {
        let source =
            "class Greeter {\n  greet() { return 'hi'; }\n  farewell() { return 'bye'; }\n}";
        let tree = parse(source, Language::TypeScript).unwrap();
        let fns = extract_functions(&tree, source, Language::TypeScript);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"greet"),
            "class methods should be extracted"
        );
        assert!(names.contains(&"farewell"));
    }

    // -- Case-insensitive extension matching --

    #[test]
    fn detect_language_case_insensitive() {
        assert_eq!(Language::from_extension("RS"), Some(Language::Rust));
        assert_eq!(Language::from_extension("Py"), Some(Language::Python));
        assert_eq!(Language::from_extension("TS"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("TSX"), Some(Language::Tsx));
    }

    // -- Bash support --

    #[test]
    fn detect_language_bash() {
        assert_eq!(Language::from_extension("sh"), Some(Language::Bash));
        assert_eq!(Language::from_extension("bash"), Some(Language::Bash));
        assert_eq!(Language::from_extension("zsh"), Some(Language::Bash));
    }

    #[test]
    fn detect_language_bash_from_path() {
        assert_eq!(
            Language::from_path(std::path::Path::new("deploy.sh")),
            Some(Language::Bash)
        );
    }

    #[test]
    fn parse_valid_bash() {
        let source = "#!/bin/bash\nset -euo pipefail\necho \"hello\"\n";
        let tree = parse(source, Language::Bash).unwrap();
        assert_eq!(tree.root_node().kind(), "program");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_bash_function() {
        let source = "#!/bin/bash\nmy_func() {\n  echo \"hello\"\n  return 0\n}\n";
        let tree = parse(source, Language::Bash).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn extract_functions_bash() {
        let source =
            "#!/bin/bash\nmy_func() {\n  echo \"inside\"\n}\n\nanother() {\n  return 1\n}\n";
        let tree = parse(source, Language::Bash).unwrap();
        let fns = extract_functions(&tree, source, Language::Bash);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["my_func", "another"]);
    }

    // -- Dockerfile support --

    #[test]
    fn detect_language_dockerfile_from_path() {
        assert_eq!(
            Language::from_path(std::path::Path::new("Dockerfile")),
            Some(Language::Dockerfile)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("Dockerfile.prod")),
            Some(Language::Dockerfile)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("dockerfile")),
            Some(Language::Dockerfile)
        );
    }

    #[test]
    fn detect_language_dockerfile_extension() {
        assert_eq!(
            Language::from_extension("dockerfile"),
            Some(Language::Dockerfile)
        );
    }

    #[test]
    fn parse_valid_dockerfile() {
        let source =
            "FROM node:18-alpine\nRUN npm install\nCOPY . /app\nCMD [\"node\", \"server.js\"]\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn dockerfile_no_functions() {
        let source = "FROM node:18\nRUN echo hello\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let fns = extract_functions(&tree, source, Language::Dockerfile);
        assert!(fns.is_empty());
    }

    // -- YAML support --

    #[test]
    fn detect_language_yaml() {
        assert_eq!(Language::from_extension("yaml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("yml"), Some(Language::Yaml));
        assert_eq!(Language::from_extension("YAML"), Some(Language::Yaml));
    }

    #[test]
    fn detect_language_yaml_from_path() {
        assert_eq!(
            Language::from_path(std::path::Path::new("automations.yaml")),
            Some(Language::Yaml)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("configuration.yml")),
            Some(Language::Yaml)
        );
    }

    #[test]
    fn parse_valid_yaml() {
        let source = "key: value\nlist:\n  - item1\n  - item2\n";
        let tree = parse(source, Language::Yaml).unwrap();
        assert_eq!(tree.root_node().kind(), "stream");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_yaml_automation() {
        let source = "automation:\n  - alias: Turn on lights\n    trigger:\n      - platform: state\n        entity_id: binary_sensor.motion\n    action:\n      - service: light.turn_on\n        target:\n          entity_id: light.living_room\n";
        let tree = parse(source, Language::Yaml).unwrap();
        assert!(!tree.root_node().has_error());
    }

    // -- Terraform/HCL support --
    //
    // tree-sitter-hcl AST node kinds (verified via dump test):
    //   Root: config_file
    //   Top-level: body > block | attribute
    //   Block structure: block > identifier (type), string_lit (labels), block_start, body, block_end
    //   Attribute: attribute > identifier, =, expression
    //   Values: literal_value > string_lit (quoted_template_start, template_literal, quoted_template_end)
    //           | numeric_lit | bool_lit
    //   Expressions: expression > variable_expr > identifier
    //                | function_call > identifier, function_arguments
    //                | collection_value > object | tuple
    //   Object: object > object_start, object_elem (expression = expression), object_end
    //   String interpolation: template_expr > (template_interpolation > expression)

    #[test]
    fn detect_language_terraform() {
        assert_eq!(Language::from_extension("tf"), Some(Language::Terraform));
        assert_eq!(
            Language::from_extension("tfvars"),
            Some(Language::Terraform)
        );
        assert_eq!(Language::from_extension("TF"), Some(Language::Terraform));
    }

    #[test]
    fn detect_language_terraform_from_path() {
        assert_eq!(
            Language::from_path(std::path::Path::new("main.tf")),
            Some(Language::Terraform)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("modules/vpc/variables.tf")),
            Some(Language::Terraform)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("terraform.tfvars")),
            Some(Language::Terraform)
        );
    }

    #[test]
    fn parse_valid_terraform() {
        let source = r#"resource "aws_s3_bucket" "example" {
  bucket = "my-bucket"
  tags = {
    Name = "My bucket"
  }
}
"#;
        let tree = parse(source, Language::Terraform).unwrap();
        assert_eq!(tree.root_node().kind(), "config_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn terraform_no_functions() {
        let source = r#"resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t3.micro"
}
"#;
        let tree = parse(source, Language::Terraform).unwrap();
        let fns = extract_functions(&tree, source, Language::Terraform);
        assert!(fns.is_empty());
    }

    #[test]
    fn detect_language_go() {
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("GO"), Some(Language::Go));
    }

    #[test]
    fn detect_language_go_from_path() {
        assert_eq!(
            Language::from_path(std::path::Path::new("main.go")),
            Some(Language::Go)
        );
        assert_eq!(
            Language::from_path(std::path::Path::new("pkg/server/handler.go")),
            Some(Language::Go)
        );
    }

    #[test]
    fn parse_valid_go() {
        let source = "package main\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
        let tree = parse(source, Language::Go).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn extract_functions_go() {
        let source = "package main\n\nfunc foo() {}\nfunc bar() {}\n";
        let tree = parse(source, Language::Go).unwrap();
        let fns = extract_functions(&tree, source, Language::Go);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar"]);
    }

    #[test]
    fn extract_functions_go_methods() {
        let source = "package main\n\ntype Server struct{}\n\nfunc (s *Server) Serve() {}\nfunc (s *Server) Stop() {}\n";
        let tree = parse(source, Language::Go).unwrap();
        let fns = extract_functions(&tree, source, Language::Go);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Serve", "Stop"]);
    }
}

#[cfg(test)]
mod cpp_tests {
    use super::*;

    /// C and C++ share one variant: tree-sitter-cpp parses C as a subset, and
    /// `.h` is genuinely ambiguous between the two. Splitting the enum would
    /// force a guess on the single most common extension.
    #[test]
    fn cpp_and_c_extensions_map_to_cpp() {
        for ext in ["c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx"] {
            assert_eq!(
                Language::from_extension(ext),
                Some(Language::Cpp),
                "extension .{ext} must map to Cpp"
            );
        }
    }

    #[test]
    fn cpp_extensions_are_case_insensitive() {
        for ext in ["C", "H", "CPP", "Cc", "HPP"] {
            assert_eq!(
                Language::from_extension(ext),
                Some(Language::Cpp),
                "extension .{ext} must map to Cpp regardless of case"
            );
        }
    }

    #[test]
    fn from_path_routes_cpp_sources() {
        for p in ["src/main.cpp", "lib/BleFingerprint.h", "a/b/mqtt.cc"] {
            assert_eq!(
                Language::from_path(Path::new(p)),
                Some(Language::Cpp),
                "{p} must route to Cpp"
            );
        }
    }

    /// A file named `Dockerfile.h` hits the filename prefix check before the
    /// extension is consulted. Pin the precedence so adding C/C++ cannot
    /// silently steal Dockerfile routing.
    #[test]
    fn dockerfile_prefix_still_wins_over_cpp_extension() {
        assert_eq!(
            Language::from_path(Path::new("Dockerfile.h")),
            Some(Language::Dockerfile)
        );
    }

    #[test]
    fn parses_cpp_translation_unit() {
        let source = r#"
#include <cstdint>
namespace ble {
class Fingerprint {
 public:
  bool seen(int rssi) { return rssi > -90; }
};
}  // namespace ble
"#;
        let tree = parse(source, Language::Cpp).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "C++ translation unit must parse without error"
        );
    }

    /// Plain C must parse through the same grammar.
    #[test]
    fn parses_c_translation_unit() {
        let source = r#"
#include <stdio.h>
static int add(int a, int b) { return a + b; }
int main(void) { return add(1, 2); }
"#;
        let tree = parse(source, Language::Cpp).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "C translation unit must parse without error"
        );
    }

    #[test]
    fn extracts_free_functions_from_c() {
        let source =
            "static int add(int a, int b) { return a + b; }\nint main(void) { return 0; }\n";
        let tree = parse(source, Language::Cpp).unwrap();
        let fns = extract_functions(&tree, source, Language::Cpp);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["add", "main"]);
    }

    /// Every variant must report a slug and at least one extension. This is the
    /// regression guard for #483: the MCP catalog is derived from these, so a
    /// new variant cannot be added without appearing in the catalog.
    #[test]
    fn every_language_has_a_name_and_extensions() {
        for lang in Language::ALL {
            assert!(!lang.name().is_empty(), "{lang:?} must have a name");
            assert!(
                !lang.extensions().is_empty(),
                "{lang:?} must declare at least one extension"
            );
        }
    }

    /// `from_extension` is derived from `extensions()`, so the two cannot drift.
    #[test]
    fn every_declared_extension_round_trips() {
        for lang in Language::ALL {
            for ext in lang.extensions() {
                assert_eq!(
                    Language::from_extension(ext),
                    Some(*lang),
                    ".{ext} must round-trip to {lang:?}"
                );
            }
        }
    }

    /// Member functions defined inside a class body.
    #[test]
    fn extracts_member_functions_from_cpp() {
        let source = "class Fingerprint {\n public:\n  bool seen(int rssi) { return rssi > -90; }\n  void reset() {}\n};\n";
        let tree = parse(source, Language::Cpp).unwrap();
        let fns = extract_functions(&tree, source, Language::Cpp);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["seen", "reset"]);
    }

    /// Out-of-line definitions carry a `qualified_identifier`, not a plain one.
    #[test]
    fn extracts_out_of_line_method_definition() {
        let source = "void Fingerprint::reset() {}\n";
        let tree = parse(source, Language::Cpp).unwrap();
        let fns = extract_functions(&tree, source, Language::Cpp);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Fingerprint::reset"]);
    }

    /// Each pointer or reference in the return type wraps the declarator in
    /// another node, so the descent must not stop at the first level.
    #[test]
    fn extracts_functions_with_pointer_return_types() {
        let source = "char *dup(const char *s) { return 0; }\nchar **argv_of(int n) { return 0; }\nint &ref_of(int &x) { return x; }\n";
        let tree = parse(source, Language::Cpp).unwrap();
        let fns = extract_functions(&tree, source, Language::Cpp);
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["dup", "argv_of", "ref_of"]);
    }

    /// A declarator chain that never reaches a name must terminate and yield
    /// nothing rather than spinning. This is a bare declaration, not a
    /// definition, so there is no `function_definition` node to extract at all
    /// -- reaching the assertion is itself the termination evidence.
    #[test]
    fn function_without_resolvable_name_is_skipped_not_hung() {
        let source = "int (*signal(int, void (*)(int)))(int);\n";
        let tree = parse(source, Language::Cpp).unwrap();
        let fns = extract_functions(&tree, source, Language::Cpp);
        assert!(
            fns.is_empty(),
            "a declaration with no body must not yield a function, got {:?}",
            fns.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn all_contains_every_variant_exactly_once() {
        let mut seen: Vec<Language> = Vec::new();
        for lang in Language::ALL {
            assert!(!seen.contains(lang), "{lang:?} appears twice in ALL");
            seen.push(*lang);
        }
        // Bumped deliberately when a language is added.
        assert_eq!(seen.len(), 10, "ALL must list every Language variant");
    }
}
