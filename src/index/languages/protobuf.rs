use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct ProtobufSupport;

impl LanguageSupport for ProtobufSupport {
    fn extensions(&self) -> &[&str] {
        &["proto"]
    }

    fn language_name(&self) -> &str {
        "protobuf"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_proto::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, None, &mut symbols, &mut imports);
        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
            ..Default::default()
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    parent: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "package" => {
            if let Some(sym) = extract_package(node, source) {
                symbols.push(sym);
            }
        }
        "import" => {
            if let Some(imp) = extract_import(node, source) {
                imports.push(imp);
            }
        }
        "message" => {
            extract_message(node, source, parent, symbols, imports);
            return;
        }
        "enum" => {
            extract_enum(node, source, parent, symbols);
            return;
        }
        "service" => {
            extract_service(node, source, symbols);
            return;
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, parent, symbols, imports);
    }
}

fn extract_package(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|c| c.kind() == "full_ident")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Module,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_import(node: Node, source: &[u8]) -> Option<ExtractedImport> {
    let path_node = node.child_by_field_name("path")?;
    let raw = node_text(path_node, source);
    let path = unquote(&raw);
    if path.is_empty() {
        return None;
    }
    let is_public = children(node).any(|c| c.kind() == "public");
    Some(ExtractedImport {
        source: path,
        specifiers: vec![],
        is_reexport: is_public,
    })
}

#[expect(
    clippy::only_used_in_recursion,
    reason = "imports collected in nested messages"
)]
fn extract_message(
    node: Node,
    source: &[u8],
    parent: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    let name = match children(node).find(|c| c.kind() == "message_name") {
        Some(n) => node_text(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let msg_idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind: SymbolKind::Struct,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: parent,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });

    let body = match children(node).find(|c| c.kind() == "message_body") {
        Some(b) => b,
        None => return,
    };

    for child in children(body) {
        match child.kind() {
            "field" => {
                if let Some(sym) = extract_field(child, source, msg_idx) {
                    symbols.push(sym);
                }
            }
            "message" => {
                extract_message(child, source, Some(msg_idx), symbols, imports);
            }
            "enum" => {
                extract_enum(child, source, Some(msg_idx), symbols);
            }
            _ => {}
        }
    }
}

fn extract_field(node: Node, source: &[u8], parent_idx: usize) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|c| c.kind() == "identifier")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Field,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: false,
        parent_idx: Some(parent_idx),
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_enum(
    node: Node,
    source: &[u8],
    parent: Option<usize>,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let name = match children(node).find(|c| c.kind() == "enum_name") {
        Some(n) => node_text(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let enum_idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind: SymbolKind::Enum,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: parent,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });

    let body = match children(node).find(|c| c.kind() == "enum_body") {
        Some(b) => b,
        None => return,
    };

    for child in children(body) {
        if child.kind() == "enum_field"
            && let Some(sym) = extract_enum_variant(child, source, enum_idx)
        {
            symbols.push(sym);
        }
    }
}

fn extract_enum_variant(node: Node, source: &[u8], parent_idx: usize) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|c| c.kind() == "identifier")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Field,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: false,
        parent_idx: Some(parent_idx),
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_service(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let name = match children(node).find(|c| c.kind() == "service_name") {
        Some(n) => node_text(n, source),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let svc_idx = symbols.len();
    symbols.push(ExtractedSymbol {
        name,
        kind: SymbolKind::Service,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    });

    for child in children(node) {
        if child.kind() == "rpc"
            && let Some(sym) = extract_rpc(child, source, svc_idx)
        {
            symbols.push(sym);
        }
    }
}

fn extract_rpc(node: Node, source: &[u8], parent_idx: usize) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|c| c.kind() == "rpc_name")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Method,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: Some(parent_idx),
        unused_excluded: false,
        complexity: None,
        owner_type: None,
    })
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let text = std::str::from_utf8(&source[start..end]).ok()?;
    let first_line = text.lines().next().unwrap_or(text).trim();
    if first_line.is_empty() {
        return None;
    }
    let truncated = if first_line.len() > 200 {
        &first_line[..200]
    } else {
        first_line
    };
    Some(truncated.to_string())
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .to_string()
}

fn unquote(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_proto(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_proto::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = ProtobufSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_message_definition() {
        let result = parse_proto(
            r#"syntax = "proto3";
message Person {
  string name = 1;
  int32 age = 2;
  repeated string emails = 3;
}
"#,
        );
        let msg: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Struct))
            .collect();
        assert_eq!(msg.len(), 1);
        assert_eq!(msg[0].name, "Person");
        assert!(msg[0].is_exported);
        assert!(msg[0].parent_idx.is_none());

        let fields: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Field))
            .collect();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "name");
        assert_eq!(fields[1].name, "age");
        assert_eq!(fields[2].name, "emails");
        for f in &fields {
            assert_eq!(f.parent_idx, Some(0));
        }
    }

    #[test]
    fn test_enum_definition() {
        let result = parse_proto(
            r#"syntax = "proto3";
enum Status {
  UNKNOWN = 0;
  ACTIVE = 1;
  INACTIVE = 2;
}
"#,
        );
        let enums: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Enum))
            .collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name, "Status");
        assert!(enums[0].is_exported);

        let variants: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Field))
            .collect();
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].name, "UNKNOWN");
        assert_eq!(variants[1].name, "ACTIVE");
        assert_eq!(variants[2].name, "INACTIVE");
        for v in &variants {
            assert_eq!(v.parent_idx, Some(0));
        }
    }

    #[test]
    fn test_service_definition() {
        let result = parse_proto(
            r#"syntax = "proto3";
service PersonService {
  rpc GetPerson(GetPersonRequest) returns (Person);
  rpc ListPeople(ListPeopleRequest) returns (stream Person);
}
"#,
        );
        let services: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Service))
            .collect();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "PersonService");
        assert!(services[0].is_exported);

        let methods: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "GetPerson");
        assert_eq!(methods[1].name, "ListPeople");
        for m in &methods {
            assert!(m.is_exported);
            assert_eq!(m.parent_idx, Some(0));
        }
    }

    #[test]
    fn test_import_statement() {
        let result = parse_proto(
            r#"syntax = "proto3";
import "google/protobuf/timestamp.proto";
import public "other.proto";
"#,
        );
        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "google/protobuf/timestamp.proto");
        assert!(!result.imports[0].is_reexport);
        assert_eq!(result.imports[1].source, "other.proto");
        assert!(result.imports[1].is_reexport);
    }

    #[test]
    fn test_package_declaration() {
        let result = parse_proto(
            r#"syntax = "proto3";
package example.v1;
"#,
        );
        let modules: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .collect();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "example.v1");
        assert!(modules[0].is_exported);
    }

    #[test]
    fn test_nested_message() {
        let result = parse_proto(
            r#"syntax = "proto3";
message Outer {
  string id = 1;
  message Inner {
    string value = 1;
  }
  Inner data = 2;
}
"#,
        );
        let structs: Vec<_> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Struct))
            .collect();
        assert_eq!(structs.len(), 2);
        assert_eq!(structs[0].name, "Outer");
        assert!(structs[0].parent_idx.is_none());
        assert_eq!(structs[1].name, "Inner");

        let outer_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Outer")
            .unwrap();
        let inner = result.symbols.iter().find(|s| s.name == "Inner").unwrap();
        assert_eq!(inner.parent_idx, Some(outer_idx));

        let inner_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Inner")
            .unwrap();
        let value_field = result.symbols.iter().find(|s| s.name == "value").unwrap();
        assert_eq!(value_field.parent_idx, Some(inner_idx));
    }

    #[test]
    fn test_empty_file() {
        let result = parse_proto("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
        assert!(result.references.is_empty());
    }

    #[test]
    fn test_mixed_proto() {
        let result = parse_proto(
            r#"syntax = "proto3";

package api.v1;

import "google/protobuf/timestamp.proto";

message User {
  string name = 1;
  int32 age = 2;

  enum Role {
    ADMIN = 0;
    USER = 1;
  }

  Role role = 3;
}

enum Status {
  UNKNOWN = 0;
  ACTIVE = 1;
}

service UserService {
  rpc GetUser(GetUserRequest) returns (User);
  rpc CreateUser(User) returns (User);
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"api.v1"));
        assert!(names.contains(&"User"));
        assert!(names.contains(&"name"));
        assert!(names.contains(&"age"));
        assert!(names.contains(&"Role"));
        assert!(names.contains(&"ADMIN"));
        assert!(names.contains(&"USER"));
        assert!(names.contains(&"role"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"UNKNOWN"));
        assert!(names.contains(&"ACTIVE"));
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"GetUser"));
        assert!(names.contains(&"CreateUser"));

        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "google/protobuf/timestamp.proto");

        let user_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "User")
            .unwrap();
        let role_enum = result.symbols.iter().find(|s| s.name == "Role").unwrap();
        assert_eq!(role_enum.parent_idx, Some(user_idx));

        let svc_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "UserService")
            .unwrap();
        let get_user = result.symbols.iter().find(|s| s.name == "GetUser").unwrap();
        assert_eq!(get_user.parent_idx, Some(svc_idx));
    }
}
