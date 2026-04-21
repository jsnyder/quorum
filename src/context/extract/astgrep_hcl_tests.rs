use super::astgrep_hcl::*;
use chrono::{DateTime, Utc};

fn when() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap()
}

#[test]
fn extracts_variable_with_description() {
    let src = "\
variable \"cidr_block\" {
  description = \"IPv4 CIDR block for the VPC\"
  type        = string
}
";
    let chunks = extract_hcl(src, "variables.tf", "mini-tf", "abc", when()).unwrap();
    let v = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("cidr_block"))
        .expect("expected cidr_block chunk");
    assert_eq!(v.kind, super::super::types::ChunkKind::Symbol);
    assert_eq!(v.signature.as_deref(), Some("variable \"cidr_block\""));
    assert!(
        v.content.contains("IPv4 CIDR block"),
        "content = {:?}",
        v.content
    );
    assert!(v.metadata.is_exported);
    assert_eq!(v.metadata.language.as_deref(), Some("terraform"));
    assert_eq!(v.provenance.extractor, "ast-grep-hcl");
}

#[test]
fn extracts_output_with_description() {
    let src = "\
output \"vpc_id\" {
  description = \"The ID of the created VPC\"
  value       = aws_vpc.this.id
}
";
    let chunks = extract_hcl(src, "outputs.tf", "mini-tf", "abc", when()).unwrap();
    let o = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("vpc_id"))
        .expect("expected vpc_id chunk");
    assert_eq!(o.signature.as_deref(), Some("output \"vpc_id\""));
    assert!(
        o.content.contains("The ID of the created VPC"),
        "content = {:?}",
        o.content
    );
}

#[test]
fn extracts_resource_with_compound_name() {
    let src = "\
resource \"aws_vpc\" \"this\" {
  cidr_block = var.cidr_block
}
";
    let chunks = extract_hcl(src, "main.tf", "mini-tf", "abc", when()).unwrap();
    let r = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("aws_vpc.this"))
        .expect("expected aws_vpc.this chunk");
    assert_eq!(r.signature.as_deref(), Some("resource \"aws_vpc\" \"this\""));
}

#[test]
fn extracts_module_block() {
    let src = "\
module \"network\" {
  source     = \"./networking\"
  cidr_block = \"10.0.0.0/16\"
}
";
    let chunks = extract_hcl(src, "main.tf", "mini-tf", "abc", when()).unwrap();
    let m = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("network"))
        .expect("expected network module chunk");
    assert_eq!(m.signature.as_deref(), Some("module \"network\""));
    assert!(
        m.content.contains("./networking"),
        "module content should include source, got: {:?}",
        m.content
    );
}

#[test]
fn variable_without_description_falls_back_to_body() {
    let src = "\
variable \"untyped\" {
  default = \"foo\"
}
";
    let chunks = extract_hcl(src, "variables.tf", "mini-tf", "abc", when()).unwrap();
    let v = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("untyped"))
        .expect("expected untyped chunk");
    assert!(!v.content.is_empty());
    assert!(
        v.content.contains("default"),
        "expected fallback body content to contain 'default', got: {:?}",
        v.content
    );
}

#[test]
fn skips_terraform_block() {
    let src = "\
terraform {
  required_version = \">= 1.0\"
}
";
    let chunks = extract_hcl(src, "main.tf", "mini-tf", "abc", when()).unwrap();
    assert!(
        chunks.is_empty(),
        "terraform block should not produce symbol chunks, got: {:?}",
        chunks.iter().map(|c| &c.qualified_name).collect::<Vec<_>>()
    );
}

#[test]
fn skips_nested_blocks() {
    let src = "\
resource \"aws_vpc\" \"this\" {
  lifecycle {
    create_before_destroy = true
  }
}
";
    let chunks = extract_hcl(src, "main.tf", "mini-tf", "abc", when()).unwrap();
    let names: Vec<&str> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(
        names.contains(&"aws_vpc.this"),
        "expected aws_vpc.this; got {names:?}"
    );
    assert!(
        !names.contains(&"lifecycle"),
        "nested lifecycle block must not be extracted; got {names:?}"
    );
    assert_eq!(chunks.len(), 1, "only one top-level symbol expected");
}

#[test]
fn extracts_multiple_blocks_from_fixture() {
    let base =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context/repos/mini-terraform/networking");

    let main = std::fs::read_to_string(base.join("main.tf")).unwrap();
    let outs = std::fs::read_to_string(base.join("outputs.tf")).unwrap();
    let vars = std::fs::read_to_string(base.join("variables.tf")).unwrap();

    let mut all = Vec::new();
    all.extend(extract_hcl(&main, "networking/main.tf", "mini-tf", "abc", when()).unwrap());
    all.extend(extract_hcl(&outs, "networking/outputs.tf", "mini-tf", "abc", when()).unwrap());
    all.extend(extract_hcl(&vars, "networking/variables.tf", "mini-tf", "abc", when()).unwrap());

    let names: Vec<&str> = all
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();

    for expected in ["aws_vpc.this", "vpc_id", "name", "cidr_block"] {
        assert!(
            names.contains(&expected),
            "expected {expected} in extracted names; got {names:?}"
        );
    }
}

#[test]
fn line_range_covers_block() {
    let src = "\
# header comment
variable \"foo\" {
  description = \"hello\"
  type        = string
}
";
    let chunks = extract_hcl(src, "variables.tf", "mini-tf", "abc", when()).unwrap();
    let v = chunks
        .iter()
        .find(|c| c.qualified_name.as_deref() == Some("foo"))
        .unwrap();
    // `variable "foo" {` is on line 2 (1-indexed), `}` is on line 5.
    assert_eq!(v.metadata.line_range.start, 2);
    assert_eq!(v.metadata.line_range.end, 5);
}

#[test]
fn same_named_resources_with_different_types() {
    let src = "\
resource \"aws_vpc\" \"main\" {
  cidr_block = \"10.0.0.0/16\"
}

resource \"aws_subnet\" \"main\" {
  cidr_block = \"10.0.1.0/24\"
}
";
    let chunks = extract_hcl(src, "main.tf", "mini-tf", "abc", when()).unwrap();
    let names: Vec<&str> = chunks
        .iter()
        .filter_map(|c| c.qualified_name.as_deref())
        .collect();
    assert!(
        names.contains(&"aws_vpc.main"),
        "expected aws_vpc.main; got {names:?}"
    );
    assert!(
        names.contains(&"aws_subnet.main"),
        "expected aws_subnet.main; got {names:?}"
    );
    assert_eq!(chunks.len(), 2);
}
