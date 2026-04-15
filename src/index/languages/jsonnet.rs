use regex::Regex;
use std::sync::LazyLock;

use tree_sitter::Language;

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

/// Regex-based parser for Jsonnet configuration language.
///
/// No tree-sitter grammar is available for Jsonnet, so extraction is performed
/// line-by-line using compiled regex patterns, following the same approach as
/// the Dockerfile parser.
pub struct JsonnetSupport;

// Jsonnet regex patterns compiled once via LazyLock.

/// Matches `local funcName(args) = ...` declarations.
static LOCAL_FUNC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*local\s+(\w+)\s*\(").unwrap());

/// Matches `local varName = ...` declarations (no parentheses after the name).
static LOCAL_VAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*local\s+(\w+)\s*=").unwrap());

/// Matches `fieldName(args):: value` hidden method declarations.
static HIDDEN_METHOD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\w+)\s*\(.*?\)\s*::").unwrap());

/// Matches `fieldName:: value` hidden field declarations.
static HIDDEN_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\w+)\s*::").unwrap());

/// Matches `fieldName: value` visible object field declarations.
/// Uses `:[^:]` instead of look-ahead (unsupported by the regex crate).
static FIELD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\w+)\s*:[^:]").unwrap());

/// Matches `import "path"` statements.
static IMPORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"import\s+["']([^"']+)["']"#).unwrap());

/// Matches `importstr "path"` statements.
static IMPORTSTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"importstr\s+["']([^"']+)["']"#).unwrap());

/// Matches top-level `function(args) { ... }` pattern.
static TOP_FUNC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*function\s*\(").unwrap());

impl LanguageSupport for JsonnetSupport {
    fn extensions(&self) -> &[&str] {
        &["jsonnet", "libsonnet"]
    }

    fn language_name(&self) -> &str {
        "jsonnet"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        // Jsonnet uses regex-based parsing; return YAML as a no-op
        // language so the tree-sitter machinery doesn't error.
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], _tree: &tree_sitter::Tree) -> ParseResult {
        let text = std::str::from_utf8(source).unwrap_or("");
        let mut symbols = Vec::new();
        let mut imports = Vec::new();

        for (line_idx, line) in text.lines().enumerate() {
            let line_num = line_idx as u32 + 1;

            // Skip blank lines and single-line comments
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
                continue;
            }

            // Local function: `local funcName(args) = ...`
            if let Some(cap) = LOCAL_FUNC_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(trimmed.to_string()),
                    is_exported: false,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
            // Local variable: `local varName = ...`
            else if let Some(cap) = LOCAL_VAR_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(trimmed.to_string()),
                    is_exported: false,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
            // Hidden method: `fieldName(args):: value`
            else if let Some(cap) = HIDDEN_METHOD_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(trimmed.to_string()),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
            // Hidden field: `fieldName:: value`
            else if let Some(cap) = HIDDEN_FIELD_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(trimmed.to_string()),
                    is_exported: false,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
            // Visible object field: `fieldName: value`
            else if let Some(cap) = FIELD_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(trimmed.to_string()),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }
            // Top-level function: `function(args) { ... }`
            else if TOP_FUNC_RE.is_match(line) {
                symbols.push(ExtractedSymbol {
                    name: "main".to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(trimmed.to_string()),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }

            // Import statements can appear anywhere on a line
            for cap in IMPORT_RE.captures_iter(line) {
                imports.push(ExtractedImport {
                    source: cap[1].to_string(),
                    specifiers: Vec::new(),
                    is_reexport: false,
                });
            }
            for cap in IMPORTSTR_RE.captures_iter(line) {
                imports.push(ExtractedImport {
                    source: cap[1].to_string(),
                    specifiers: Vec::new(),
                    is_reexport: false,
                });
            }
        }

        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_jsonnet(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = JsonnetSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_local_function() {
        let result = parse_jsonnet("local greet(name) = 'Hello ' + name;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "greet");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_local_variable() {
        let result = parse_jsonnet("local basePort = 8080;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "basePort");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_object_field() {
        let result = parse_jsonnet("  name: 'my-service',\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "name");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_hidden_field() {
        let result = parse_jsonnet("  _config:: { replicas: 3 },\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "_config");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_import_statement() {
        let result = parse_jsonnet("local lib = import 'lib/utils.libsonnet';\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "lib/utils.libsonnet");
        assert!(!result.imports[0].is_reexport);
    }

    #[test]
    fn test_importstr() {
        let result = parse_jsonnet("local readme = importstr 'README.md';\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "README.md");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_jsonnet("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
        assert!(result.references.is_empty());
    }

    #[test]
    fn test_mixed_jsonnet() {
        let result = parse_jsonnet(
            r#"local kube = import 'kube.libsonnet';
local env = importstr 'env.txt';

local replicas(n) = { replicas: n };
local defaultPort = 8080;

{
  apiVersion: 'apps/v1',
  kind: 'Deployment',
  metadata:: { name: 'nginx' },
  deploy(port):: kube.deployment(port),
  spec: {
    containers: [
      { name: 'nginx', image: 'nginx:latest' },
    ],
  },
}
"#,
        );

        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"replicas"));
        assert!(names.contains(&"defaultPort"));
        assert!(names.contains(&"apiVersion"));
        assert!(names.contains(&"kind"));
        assert!(names.contains(&"metadata"));
        assert!(names.contains(&"deploy"));
        assert!(names.contains(&"spec"));

        // Verify function vs variable classification
        let replicas_sym = result
            .symbols
            .iter()
            .find(|s| s.name == "replicas")
            .unwrap();
        assert!(matches!(replicas_sym.kind, SymbolKind::Function));
        assert!(!replicas_sym.is_exported);

        let default_port = result
            .symbols
            .iter()
            .find(|s| s.name == "defaultPort")
            .unwrap();
        assert!(matches!(default_port.kind, SymbolKind::Variable));
        assert!(!default_port.is_exported);

        let api_version = result
            .symbols
            .iter()
            .find(|s| s.name == "apiVersion")
            .unwrap();
        assert!(matches!(api_version.kind, SymbolKind::Variable));
        assert!(api_version.is_exported);

        let metadata = result
            .symbols
            .iter()
            .find(|s| s.name == "metadata")
            .unwrap();
        assert!(!metadata.is_exported);

        let deploy = result.symbols.iter().find(|s| s.name == "deploy").unwrap();
        assert!(matches!(deploy.kind, SymbolKind::Function));
        assert!(deploy.is_exported);

        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].source, "kube.libsonnet");
        assert_eq!(result.imports[1].source, "env.txt");
    }
}

// Rust guideline compliant 2026-04-13
