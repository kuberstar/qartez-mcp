use regex::Regex;
use std::sync::LazyLock;

use tree_sitter::Language;

use super::LanguageSupport;
use crate::index::symbols::{
    ExtractedReference, ExtractedSymbol, ParseResult, ReferenceKind, SymbolKind,
};

pub struct DockerfileSupport;

// No compatible tree-sitter-dockerfile crate for tree-sitter 0.24+.
// Dockerfile syntax is line-oriented, so regex works well here.
static FROM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^FROM\s+(\S+)(?:\s+[Aa][Ss]\s+(\S+))?").unwrap());
static ARG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^ARG\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());
static ENV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^ENV\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());
static EXPOSE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^EXPOSE\s+(.+)$").unwrap());
static ENTRYPOINT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(ENTRYPOINT|CMD)\s+(.+)$").unwrap());
static COPY_FROM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^COPY\s+--from=(\S+)").unwrap());

impl LanguageSupport for DockerfileSupport {
    fn extensions(&self) -> &[&str] {
        &["dockerfile"]
    }

    fn filenames(&self) -> &[&str] {
        &["Dockerfile", "dockerfile"]
    }

    fn filename_prefixes(&self) -> &[&str] {
        &["Dockerfile."]
    }

    fn language_name(&self) -> &str {
        "dockerfile"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        // Dockerfile uses regex-based parsing; return YAML as a no-op
        // language so the tree-sitter machinery doesn't error.
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], _tree: &tree_sitter::Tree) -> ParseResult {
        let text = std::str::from_utf8(source).unwrap_or("");
        let mut symbols = Vec::new();
        let mut references = Vec::new();
        let mut stage_names: Vec<String> = Vec::new();

        // Join continuation lines (backslash + newline) before processing
        let joined = text.replace("\\\n", " ");

        for (line_idx, line) in joined.lines().enumerate() {
            let line = line.trim();
            let line_num = line_idx as u32 + 1;

            if let Some(cap) = FROM_RE.captures(line) {
                let image = cap[1].to_string();
                if let Some(alias) = cap.get(2) {
                    let name = alias.as_str().to_string();
                    stage_names.push(name.clone());
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Stage,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(line.to_string()),
                        is_exported: true,
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                } else {
                    symbols.push(ExtractedSymbol {
                        name: image,
                        kind: SymbolKind::Stage,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(line.to_string()),
                        is_exported: true,
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                }
            } else if let Some(cap) = ARG_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(line.to_string()),
                    is_exported: false,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = ENV_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(line.to_string()),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = EXPOSE_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: format!("EXPOSE {}", &cap[1]),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(line.to_string()),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = ENTRYPOINT_RE.captures(line) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(line.to_string()),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            }

            // COPY --from= cross-stage reference
            if let Some(cap) = COPY_FROM_RE.captures(line) {
                let from_name = cap[1].to_string();
                if stage_names.contains(&from_name) {
                    references.push(ExtractedReference {
                        name: from_name,
                        line: line_num,
                        from_symbol_idx: None,
                        kind: ReferenceKind::Use,
                        qualifier: None,
                        receiver_type_hint: None,
                    });
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_dockerfile(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = DockerfileSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_from_image() {
        let result = parse_dockerfile("FROM ubuntu:22.04\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "ubuntu:22.04");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Stage));
    }

    #[test]
    fn test_multi_stage_build() {
        let result = parse_dockerfile(
            "FROM rust:1.75 AS builder\nRUN cargo build\nFROM debian:bookworm-slim\nCOPY --from=builder /app /app\n",
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"builder"));
        assert!(names.contains(&"debian:bookworm-slim"));
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].name, "builder");
    }

    #[test]
    fn test_arg() {
        let result = parse_dockerfile("ARG VERSION=1.0\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "VERSION");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
    }

    #[test]
    fn test_env() {
        let result = parse_dockerfile("ENV APP_PORT=8080\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "APP_PORT");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_expose() {
        let result = parse_dockerfile("EXPOSE 8080\n");
        assert_eq!(result.symbols.len(), 1);
        assert!(result.symbols[0].name.contains("8080"));
    }

    #[test]
    fn test_entrypoint_and_cmd() {
        let result = parse_dockerfile("ENTRYPOINT [\"/app\"]\nCMD [\"--port\", \"8080\"]\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ENTRYPOINT"));
        assert!(names.contains(&"CMD"));
    }

    #[test]
    fn test_full_dockerfile() {
        let result = parse_dockerfile(
            r#"FROM rust:1.75 AS builder
ARG BUILD_MODE=release
ENV RUST_LOG=info
WORKDIR /app
COPY . .
RUN cargo build --release
EXPOSE 8080
FROM debian:bookworm-slim
COPY --from=builder /app/target/release/myapp /usr/local/bin/
ENTRYPOINT ["myapp"]
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"builder"));
        assert!(names.contains(&"BUILD_MODE"));
        assert!(names.contains(&"RUST_LOG"));
        assert!(names.contains(&"ENTRYPOINT"));
        assert!(names.iter().any(|n| n.contains("8080")));
    }
}
