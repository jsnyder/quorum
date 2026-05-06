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
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Language::Rust),
            "py" => Some(Language::Python),
            "ts" | "js" | "mjs" | "cjs" => Some(Language::TypeScript),
            "tsx" | "jsx" => Some(Language::Tsx),
            "yaml" | "yml" => Some(Language::Yaml),
            "sh" | "bash" | "zsh" | "bats" => Some(Language::Bash),
            "dockerfile" => Some(Language::Dockerfile),
            "tf" | "tfvars" => Some(Language::Terraform),
            _ => None,
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        // Check filename first for extensionless files (Dockerfile, Dockerfile.prod, etc.)
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.to_lowercase().starts_with("dockerfile") {
                return Some(Language::Dockerfile);
            }
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
            if kinds.contains(&node.kind()) {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = &source[name_node.byte_range()];
                    functions.push(FunctionInfo {
                        name: name.to_string(),
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                    });
                }
            }

            // Arrow functions: const name = (...) => { ... }
            // Tree shape: lexical_declaration > variable_declarator[name, value=arrow_function]
            if is_ts && node.kind() == "arrow_function" {
                if let Some(parent) = node.parent() {
                    if parent.kind() == "variable_declarator" {
                        if let Some(name_node) = parent.child_by_field_name("name") {
                            let name = &source[name_node.byte_range()];
                            functions.push(FunctionInfo {
                                name: name.to_string(),
                                line_start: parent.start_position().row as u32 + 1,
                                line_end: node.end_position().row as u32 + 1,
                            });
                        }
                    }
                }
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
}
