use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedImport, ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct HclSupport;

impl LanguageSupport for HclSupport {
    fn extensions(&self) -> &[&str] {
        &["tf"]
    }

    fn language_name(&self) -> &str {
        "hcl"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_hcl::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut references = Vec::new();
        let root = tree.root_node();
        extract_blocks(root, source, &mut symbols);
        extract_module_sources(root, source, &mut imports);
        extract_terraform_refs(root, source, &symbols, &mut references);
        ParseResult {
            symbols,
            imports,
            references,
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_blocks(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(node) {
        if child.kind() == "block" {
            extract_block(child, source, symbols);
        } else if child.kind() == "body" {
            extract_blocks(child, source, symbols);
        }
    }
}

fn extract_block(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let mut child_iter = children(node).peekable();

    let block_type_node = match child_iter.next() {
        Some(n) if n.kind() == "identifier" => n,
        _ => return,
    };
    let block_type = node_text(block_type_node, source);

    let mut labels = Vec::new();
    for child in child_iter {
        match child.kind() {
            "string_lit" => {
                if let Some(text) = extract_string_content(child, source) {
                    labels.push(text);
                }
            }
            "block_start" | "block_end" | "body" => break,
            _ => {}
        }
    }

    match block_type.as_str() {
        "resource" => {
            if labels.len() >= 2 {
                let name = format!("{}.{}", labels[0], labels[1]);
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Resource,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        "data" => {
            if labels.len() >= 2 {
                let name = format!("data.{}.{}", labels[0], labels[1]);
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Data,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        "variable" => {
            if let Some(name) = labels.first() {
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    kind: SymbolKind::Variable,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        "output" => {
            if let Some(name) = labels.first() {
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    kind: SymbolKind::Output,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        "module" => {
            if let Some(name) = labels.first() {
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    kind: SymbolKind::Module,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        "provider" => {
            if let Some(name) = labels.first() {
                symbols.push(ExtractedSymbol {
                    name: name.clone(),
                    kind: SymbolKind::Provider,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    signature: extract_signature(node, source),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }
        "locals" => {
            extract_locals(node, source, symbols);
        }
        _ => {}
    }
}

fn extract_locals(block_node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    for child in children(block_node) {
        if child.kind() == "body" {
            for attr in children(child) {
                if attr.kind() == "attribute" {
                    let name_node = children(attr).find(|n| n.kind() == "identifier");
                    if let Some(name_node) = name_node {
                        let name = node_text(name_node, source);
                        if !name.is_empty() {
                            symbols.push(ExtractedSymbol {
                                name,
                                kind: SymbolKind::Local,
                                line_start: attr.start_position().row as u32 + 1,
                                line_end: attr.end_position().row as u32 + 1,
                                signature: extract_signature(attr, source),
                                is_exported: false,
                                parent_idx: None,
                                unused_excluded: false,
                                complexity: None,
                            });
                        }
                    }
                }
            }
        }
    }
}

fn extract_string_content(node: Node, source: &[u8]) -> Option<String> {
    for child in children(node) {
        if child.kind() == "template_literal" {
            let text = node_text(child, source);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let text = std::str::from_utf8(&source[start..end]).ok()?;

    let sig = if let Some(brace_pos) = text.find('{') {
        text[..brace_pos].trim()
    } else {
        text.lines().next().unwrap_or(text).trim()
    };

    if sig.is_empty() {
        return None;
    }

    let truncated = if sig.len() > 200 { &sig[..200] } else { sig };
    Some(truncated.to_string())
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .to_string()
}

/// Extract `module` block `source` attributes as import edges.
fn extract_module_sources(
    node: Node,
    source: &[u8],
    imports: &mut Vec<ExtractedImport>,
) {
    for child in children(node) {
        if child.kind() == "block" {
            let block_type = children(child)
                .find(|n| n.kind() == "identifier")
                .map(|n| node_text(n, source));
            if block_type.as_deref() == Some("module") {
                // Look inside the block body for `source = "..."`
                for body_child in children(child) {
                    if body_child.kind() == "body" {
                        for attr in children(body_child) {
                            if attr.kind() == "attribute" {
                                let attr_name = children(attr)
                                    .find(|n| n.kind() == "identifier")
                                    .map(|n| node_text(n, source));
                                if attr_name.as_deref() == Some("source")
                                    && let Some(val) = extract_attr_string_value(attr, source)
                                        && (val.starts_with("./") || val.starts_with("../")) {
                                            imports.push(ExtractedImport {
                                                source: val,
                                                specifiers: vec![],
                                                is_reexport: false,
                                            });
                                        }
                            }
                        }
                    }
                }
            }
        } else if child.kind() == "body" {
            extract_module_sources(child, source, imports);
        }
    }
}

fn extract_attr_string_value(attr: Node, source: &[u8]) -> Option<String> {
    // The path is: attribute -> expression -> literal_value -> string_lit -> template_literal
    find_string_recursive(attr, source)
}

fn find_string_recursive(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() == "template_literal" {
        let text = node_text(node, source);
        if !text.is_empty() {
            return Some(text);
        }
    }
    for child in children(node) {
        if let Some(found) = find_string_recursive(child, source) {
            return Some(found);
        }
    }
    None
}

// Cached regex patterns for Terraform cross-file reference extraction.
static VAR_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"var\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap());
static LOCAL_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"local\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap());
static MODULE_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"module\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap());
static DATA_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"data\.([a-zA-Z_][a-zA-Z0-9_]*)\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap()
});
static RESOURCE_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^.a-zA-Z_])([a-zA-Z_][a-zA-Z0-9_]*)\.([a-zA-Z_][a-zA-Z0-9_]*)\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap()
});

/// Scan all attribute values for Terraform references like `var.region`,
/// `local.name_prefix`, `module.vpc.vpc_id`, `data.aws_ami.ubuntu.id`,
/// `aws_instance.web.id`.
fn extract_terraform_refs(
    node: Node,
    source: &[u8],
    symbols: &[ExtractedSymbol],
    references: &mut Vec<ExtractedReference>,
) {
    collect_attr_refs(node, source, symbols, references);
}

fn collect_attr_refs(
    node: Node,
    source: &[u8],
    symbols: &[ExtractedSymbol],
    references: &mut Vec<ExtractedReference>,
) {
    if node.kind() == "attribute" {
        // Get the value side of the attribute (skip the identifier = part)
        let text = node_text(node, source);
        let line = node.start_position().row as u32 + 1;

        // Find enclosing symbol index for this attribute
        let enclosing_idx = symbols
            .iter()
            .position(|s| s.line_start <= line && s.line_end >= line);

        // Look for var.xxx
        if let Some(eq_pos) = text.find('=') {
            let value_part = &text[eq_pos + 1..];
            extract_refs_from_text(value_part, line, enclosing_idx, symbols, references);
        }
    }

    for child in children(node) {
        collect_attr_refs(child, source, symbols, references);
    }
}

fn extract_refs_from_text(
    text: &str,
    line: u32,
    enclosing_idx: Option<usize>,
    symbols: &[ExtractedSymbol],
    references: &mut Vec<ExtractedReference>,
) {
    // var.xxx -> references variable "xxx"
    for cap in VAR_REF.captures_iter(text) {
        let var_name = &cap[1];
        if symbols.iter().any(|s| {
            s.name == var_name && matches!(s.kind, SymbolKind::Variable)
        }) {
            references.push(ExtractedReference {
                name: var_name.to_string(),
                line,
                from_symbol_idx: enclosing_idx,
                kind: ReferenceKind::Use,
                receiver_type_hint: None,
            });
        }
    }

    // local.xxx -> references locals value "xxx"
    for cap in LOCAL_REF.captures_iter(text) {
        let local_name = &cap[1];
        if symbols.iter().any(|s| {
            s.name == local_name && matches!(s.kind, SymbolKind::Local)
        }) {
            references.push(ExtractedReference {
                name: local_name.to_string(),
                line,
                from_symbol_idx: enclosing_idx,
                kind: ReferenceKind::Use,
                receiver_type_hint: None,
            });
        }
    }

    // module.xxx -> references module "xxx"
    for cap in MODULE_REF.captures_iter(text) {
        let mod_name = &cap[1];
        if symbols.iter().any(|s| {
            s.name == mod_name && matches!(s.kind, SymbolKind::Module)
        }) {
            references.push(ExtractedReference {
                name: mod_name.to_string(),
                line,
                from_symbol_idx: enclosing_idx,
                kind: ReferenceKind::Use,
                receiver_type_hint: None,
            });
        }
    }

    // data.type.name -> references data source "data.type.name"
    for cap in DATA_REF.captures_iter(text) {
        let data_name = format!("data.{}.{}", &cap[1], &cap[2]);
        if symbols.iter().any(|s| s.name == data_name) {
            references.push(ExtractedReference {
                name: data_name,
                line,
                from_symbol_idx: enclosing_idx,
                kind: ReferenceKind::Use,
                receiver_type_hint: None,
            });
        }
    }

    // resource_type.name -> references resource "resource_type.name"
    for cap in RESOURCE_REF.captures_iter(text) {
        let type_name = &cap[1];
        let res_name = &cap[2];
        // Skip known prefixes that aren't resources
        if matches!(
            type_name,
            "var" | "local" | "module" | "data" | "path" | "terraform" | "self" | "each" | "count"
        ) {
            continue;
        }
        let resource_ref = format!("{type_name}.{res_name}");
        if symbols.iter().any(|s| {
            s.name == resource_ref && matches!(s.kind, SymbolKind::Resource)
        }) {
            references.push(ExtractedReference {
                name: resource_ref,
                line,
                from_symbol_idx: enclosing_idx,
                kind: ReferenceKind::Use,
                receiver_type_hint: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_hcl(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_hcl::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = HclSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_resource() {
        let result = parse_hcl(
            r#"resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t2.micro"
}"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "aws_instance.web");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Resource));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_variable() {
        let result = parse_hcl(
            r#"variable "region" {
  type    = string
  default = "us-east-1"
}"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "region");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_output() {
        let result = parse_hcl(
            r#"output "instance_ip" {
  value = aws_instance.web.public_ip
}"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "instance_ip");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Output));
    }

    #[test]
    fn test_data_source() {
        let result = parse_hcl(
            r#"data "aws_ami" "ubuntu" {
  most_recent = true
}"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "data.aws_ami.ubuntu");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Data));
    }

    #[test]
    fn test_module() {
        let result = parse_hcl(
            r#"module "vpc" {
  source = "./modules/vpc"
}"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "vpc");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Module));
    }

    #[test]
    fn test_provider() {
        let result = parse_hcl(
            r#"provider "aws" {
  region = "us-east-1"
}"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "aws");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Provider));
    }

    #[test]
    fn test_locals() {
        let result = parse_hcl(
            r#"locals {
  environment = "production"
  project     = "myapp"
}"#,
        );
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "environment");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Local));
        assert!(!result.symbols[0].is_exported);
        assert_eq!(result.symbols[1].name, "project");
    }

    #[test]
    fn test_mixed_terraform() {
        let result = parse_hcl(
            r#"
provider "aws" {
  region = "us-east-1"
}

variable "env" {
  type = string
}

locals {
  name_prefix = "myapp"
}

resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

data "aws_availability_zones" "available" {}

module "subnet" {
  source = "./modules/subnet"
}

output "vpc_id" {
  value = aws_vpc.main.id
}
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"aws"));
        assert!(names.contains(&"env"));
        assert!(names.contains(&"name_prefix"));
        assert!(names.contains(&"aws_vpc.main"));
        assert!(names.contains(&"data.aws_availability_zones.available"));
        assert!(names.contains(&"subnet"));
        assert!(names.contains(&"vpc_id"));
        assert_eq!(result.symbols.len(), 7);
    }

    #[test]
    fn test_line_numbers() {
        let result = parse_hcl(
            r#"variable "a" {
  type = string
}

resource "aws_s3_bucket" "b" {
  bucket = "my-bucket"
  acl    = "private"
}"#,
        );
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].line_start, 1);
        assert_eq!(result.symbols[0].line_end, 3);
        assert_eq!(result.symbols[1].line_start, 5);
        assert_eq!(result.symbols[1].line_end, 8);
    }

    #[test]
    fn test_no_imports() {
        let result = parse_hcl(r#"resource "null_resource" "example" {}"#);
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_var_reference() {
        let result = parse_hcl(
            r#"variable "region" {
  type = string
}

resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = var.region
}"#,
        );
        assert!(!result.references.is_empty());
        assert!(result.references.iter().any(|r| r.name == "region"));
    }

    #[test]
    fn test_local_reference() {
        let result = parse_hcl(
            r#"locals {
  name_prefix = "myapp"
}

resource "aws_s3_bucket" "main" {
  bucket = local.name_prefix
}"#,
        );
        assert!(result.references.iter().any(|r| r.name == "name_prefix"));
    }

    #[test]
    fn test_resource_reference() {
        let result = parse_hcl(
            r#"resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "sub" {
  vpc_id = aws_vpc.main.id
}"#,
        );
        assert!(result
            .references
            .iter()
            .any(|r| r.name == "aws_vpc.main"));
    }

    #[test]
    fn test_data_reference() {
        let result = parse_hcl(
            r#"data "aws_ami" "ubuntu" {
  most_recent = true
}

resource "aws_instance" "web" {
  ami = data.aws_ami.ubuntu.id
}"#,
        );
        assert!(result
            .references
            .iter()
            .any(|r| r.name == "data.aws_ami.ubuntu"));
    }

    #[test]
    fn test_module_source_import() {
        let result = parse_hcl(
            r#"module "vpc" {
  source = "./modules/vpc"
}"#,
        );
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "./modules/vpc");
    }

    #[test]
    fn test_module_reference() {
        let result = parse_hcl(
            r#"module "vpc" {
  source = "./modules/vpc"
}

resource "aws_subnet" "sub" {
  vpc_id = module.vpc.vpc_id
}"#,
        );
        assert!(result.references.iter().any(|r| r.name == "vpc"));
    }
}
