use regex::Regex;
use std::sync::LazyLock;

use tree_sitter::Language;

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct HelmSupport;

// No maintained tree-sitter-gotmpl crate exists, so this parser uses regex
// to extract Go template constructs from .tpl files.
static DEFINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\{\{-?\s*define\s+"([^"]+)"\s*-?\}\}"#).unwrap());
static INCLUDE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\{\{-?\s*(?:include|template)\s+"([^"]+)""#).unwrap());

impl LanguageSupport for HelmSupport {
    fn extensions(&self) -> &[&str] {
        &["tpl"]
    }

    fn language_name(&self) -> &str {
        "helm"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        // .tpl files are parsed as YAML since the Go template syntax
        // interleaves with YAML. We use regex for the template constructs
        // and tree-sitter-yaml for any YAML structure.
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], _tree: &tree_sitter::Tree) -> ParseResult {
        let text = std::str::from_utf8(source).unwrap_or("");
        let mut symbols = Vec::new();
        let mut references = Vec::new();
        let mut defined_names: Vec<String> = Vec::new();

        // Extract {{ define "name" }} blocks
        for cap in DEFINE_RE.captures_iter(text) {
            let name = cap[1].to_string();
            let match_obj = cap.get(0).unwrap();
            let line = text[..match_obj.start()]
                .chars()
                .filter(|&c| c == '\n')
                .count() as u32
                + 1;

            // Find matching {{ end }}
            let end_line = find_end_line(text, match_obj.end(), line);

            defined_names.push(name.clone());
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Function,
                line_start: line,
                line_end: end_line,
                signature: Some(match_obj.as_str().to_string()),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
                owner_type: None,
            });
        }

        // Extract {{ include "name" }} and {{ template "name" }} references
        for cap in INCLUDE_RE.captures_iter(text) {
            let name = cap[1].to_string();
            let match_obj = cap.get(0).unwrap();
            let line = text[..match_obj.start()]
                .chars()
                .filter(|&c| c == '\n')
                .count() as u32
                + 1;

            // Only create references, not symbols, for include/template
            if !defined_names.contains(&name) {
                references.push(ExtractedReference {
                    name,
                    line,
                    from_symbol_idx: None,
                    kind: ReferenceKind::Call,
                    qualifier: None,
                    receiver_type_hint: None,
                });
            }
        }

        ParseResult {
            symbols,
            imports: Vec::new(),
            references,
            ..Default::default()
        }
    }
}

fn find_end_line(text: &str, start_offset: usize, start_line: u32) -> u32 {
    // Simple heuristic: find the next {{ end }} after the define
    static END_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\{\{-?\s*end\s*-?\}\}"#).unwrap());

    if let Some(m) = END_RE.find(&text[start_offset..]) {
        let abs_pos = start_offset + m.end();
        let extra_lines = text[start_offset..abs_pos]
            .chars()
            .filter(|&c| c == '\n')
            .count() as u32;
        start_line + extra_lines
    } else {
        start_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_helm(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = HelmSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_define_block() {
        let result = parse_helm(
            r#"{{- define "mychart.labels" -}}
app: {{ .Chart.Name }}
{{- end -}}
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "mychart.labels");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_include_reference() {
        let result = parse_helm(
            r#"metadata:
  labels:
    {{- include "mychart.labels" . | nindent 4 }}
"#,
        );
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].name, "mychart.labels");
    }

    #[test]
    fn test_multiple_defines() {
        let result = parse_helm(
            r#"{{- define "mychart.name" -}}
{{ .Chart.Name }}
{{- end -}}

{{- define "mychart.fullname" -}}
{{ .Release.Name }}-{{ .Chart.Name }}
{{- end -}}
"#,
        );
        assert_eq!(result.symbols.len(), 2);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"mychart.name"));
        assert!(names.contains(&"mychart.fullname"));
    }

    #[test]
    fn test_template_reference() {
        let result = parse_helm(
            r#"{{ template "mychart.fullname" . }}
"#,
        );
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].name, "mychart.fullname");
    }
}
