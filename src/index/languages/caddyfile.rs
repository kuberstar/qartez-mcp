use regex::Regex;
use std::sync::LazyLock;

use tree_sitter::Language;

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct CaddyfileSupport;

// Caddyfile is a line-oriented configuration format with brace-delimited
// blocks. No tree-sitter grammar exists, so this parser uses regex
// (same strategy as the Dockerfile parser).

static SNIPPET_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\((\w+)\)\s*\{").unwrap());

static HANDLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+(handle(?:_path)?)\s+(\S+)").unwrap());

static REVERSE_PROXY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+reverse_proxy\s+(.+)$").unwrap());

static RESPOND_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s+respond\s+(.+)$").unwrap());

static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*import\s+(.+)$").unwrap());

static NAMED_MATCHER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s+@(\w+)").unwrap());

static DIRECTIVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+(tls|log|encode)\b(.*)$").unwrap());

// Matches a site address at depth 0: hostname with dot/colon, or `localhost`,
// or a bare port like `:8080`.
static SITE_ADDR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+)\s*\{?\s*$").unwrap());

// Caddy directive keywords that should not be treated as site addresses.
const CADDY_DIRECTIVES: &[&str] = &[
    "tls",
    "log",
    "encode",
    "handle",
    "handle_path",
    "reverse_proxy",
    "respond",
    "redir",
    "rewrite",
    "header",
    "basicauth",
    "file_server",
    "root",
    "php_fastcgi",
    "templates",
    "try_files",
    "import",
    "route",
    "vars",
    "bind",
    "abort",
    "error",
    "map",
    "acme_server",
    "metrics",
    "tracing",
];

/// Returns true when `token` looks like a Caddyfile site address rather than
/// a directive keyword. Site addresses contain a dot, a colon, or are the
/// literal string `localhost`.
fn is_site_address(token: &str) -> bool {
    if CADDY_DIRECTIVES.contains(&token) {
        return false;
    }
    // Parenthesized tokens are snippet definitions, not sites.
    if token.starts_with('(') {
        return false;
    }
    token.contains('.')
        || token.contains(':')
        || token == "localhost"
        || token == "http://"
        || token.starts_with("http://")
        || token.starts_with("https://")
}

impl LanguageSupport for CaddyfileSupport {
    fn extensions(&self) -> &[&str] {
        &["caddyfile"]
    }

    fn filenames(&self) -> &[&str] {
        &["Caddyfile"]
    }

    fn language_name(&self) -> &str {
        "caddyfile"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        // Caddyfile uses regex-based parsing; return YAML as a no-op
        // language so the tree-sitter machinery does not error.
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], _tree: &tree_sitter::Tree) -> ParseResult {
        let text = std::str::from_utf8(source).unwrap_or("");
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut depth: u32 = 0;
        let mut current_site_idx: Option<usize> = None;

        for (line_idx, raw_line) in text.lines().enumerate() {
            let line_num = line_idx as u32 + 1;
            let trimmed = raw_line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Track brace depth changes on this line.
            let open_braces = raw_line.chars().filter(|&c| c == '{').count() as u32;
            let close_braces = raw_line.chars().filter(|&c| c == '}').count() as u32;

            // --- Depth-0 constructs ---
            if depth == 0 {
                // Snippet definition: (snippet_name) {
                if let Some(cap) = SNIPPET_RE.captures(raw_line) {
                    symbols.push(ExtractedSymbol {
                        name: format!("({})", &cap[1]),
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
                    depth = depth
                        .saturating_add(open_braces)
                        .saturating_sub(close_braces);
                    continue;
                }

                // Import at the global level.
                if let Some(cap) = IMPORT_RE.captures(raw_line) {
                    imports.push(ExtractedImport {
                        source: cap[1].trim().to_string(),
                        specifiers: Vec::new(),
                        is_reexport: false,
                    });
                    depth = depth
                        .saturating_add(open_braces)
                        .saturating_sub(close_braces);
                    continue;
                }

                // Site address: a token at depth 0 that looks like a hostname.
                if let Some(cap) = SITE_ADDR_RE.captures(trimmed) {
                    let addr = &cap[1];
                    if is_site_address(addr) {
                        current_site_idx = Some(symbols.len());
                        symbols.push(ExtractedSymbol {
                            name: addr.to_string(),
                            kind: SymbolKind::Class,
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
                }
            } else {
                // --- Depth 1+ constructs (inside a site/snippet block) ---

                if let Some(cap) = HANDLE_RE.captures(raw_line) {
                    symbols.push(ExtractedSymbol {
                        name: format!("{} {}", &cap[1], &cap[2]),
                        kind: SymbolKind::Function,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(trimmed.to_string()),
                        is_exported: true,
                        parent_idx: current_site_idx,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                } else if let Some(cap) = REVERSE_PROXY_RE.captures(raw_line) {
                    symbols.push(ExtractedSymbol {
                        name: format!("reverse_proxy {}", cap[1].trim()),
                        kind: SymbolKind::Variable,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(trimmed.to_string()),
                        is_exported: true,
                        parent_idx: current_site_idx,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                } else if let Some(cap) = RESPOND_RE.captures(raw_line) {
                    symbols.push(ExtractedSymbol {
                        name: format!("respond {}", cap[1].trim()),
                        kind: SymbolKind::Variable,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(trimmed.to_string()),
                        is_exported: true,
                        parent_idx: current_site_idx,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                } else if let Some(cap) = IMPORT_RE.captures(raw_line) {
                    imports.push(ExtractedImport {
                        source: cap[1].trim().to_string(),
                        specifiers: Vec::new(),
                        is_reexport: false,
                    });
                } else if let Some(cap) = NAMED_MATCHER_RE.captures(raw_line) {
                    symbols.push(ExtractedSymbol {
                        name: format!("@{}", &cap[1]),
                        kind: SymbolKind::Variable,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(trimmed.to_string()),
                        is_exported: true,
                        parent_idx: current_site_idx,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                } else if let Some(cap) = DIRECTIVE_RE.captures(raw_line) {
                    let directive = &cap[1];
                    let args = cap.get(2).map_or("", |m| m.as_str()).trim();
                    let name = if args.is_empty() {
                        directive.to_string()
                    } else {
                        format!("{directive} {args}")
                    };
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Variable,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(trimmed.to_string()),
                        is_exported: true,
                        parent_idx: current_site_idx,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                }
            }

            depth = depth
                .saturating_add(open_braces)
                .saturating_sub(close_braces);

            // Update the end line of the current site block when we return
            // to depth 0.
            if depth == 0
                && let Some(idx) = current_site_idx.take()
            {
                symbols[idx].line_end = line_num;
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

    fn parse_caddyfile(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = CaddyfileSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_site_block() {
        let result = parse_caddyfile("example.com {\n    respond \"Hello\"\n}\n");
        assert_eq!(result.symbols.len(), 2);
        assert_eq!(result.symbols[0].name, "example.com");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
        assert_eq!(result.symbols[0].line_start, 1);
        assert_eq!(result.symbols[0].line_end, 3);
    }

    #[test]
    fn test_reverse_proxy() {
        let result = parse_caddyfile("api.example.com {\n    reverse_proxy localhost:8080\n}\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"api.example.com"));
        assert!(names.iter().any(|n| n.contains("localhost:8080")));
        let rp = result
            .symbols
            .iter()
            .find(|s| s.name.contains("reverse_proxy"))
            .unwrap();
        assert!(matches!(rp.kind, SymbolKind::Variable));
    }

    #[test]
    fn test_handle_path() {
        let result = parse_caddyfile(
            "example.com {\n    handle_path /api/* {\n        reverse_proxy backend:3000\n    }\n}\n",
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names
                .iter()
                .any(|n| n.contains("handle_path") && n.contains("/api/*"))
        );
        let hp = result
            .symbols
            .iter()
            .find(|s| s.name.contains("handle_path"))
            .unwrap();
        assert!(matches!(hp.kind, SymbolKind::Function));
    }

    #[test]
    fn test_snippet() {
        let result = parse_caddyfile("(common_headers) {\n    header X-Frame-Options DENY\n}\n");
        assert_eq!(result.symbols[0].name, "(common_headers)");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_named_matcher() {
        let result = parse_caddyfile(
            "example.com {\n    @websockets {\n        header Connection *Upgrade*\n    }\n}\n",
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"@websockets"));
        let matcher = result
            .symbols
            .iter()
            .find(|s| s.name == "@websockets")
            .unwrap();
        assert!(matches!(matcher.kind, SymbolKind::Variable));
    }

    #[test]
    fn test_import() {
        let result =
            parse_caddyfile("import common_headers\nexample.com {\n    respond \"OK\"\n}\n");
        assert_eq!(result.imports.len(), 1);
        assert_eq!(result.imports[0].source, "common_headers");
    }

    #[test]
    fn test_empty_file() {
        let result = parse_caddyfile("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_caddyfile() {
        let result = parse_caddyfile(
            r#"import sites/*

(logging) {
    log {
        output file /var/log/caddy/access.log
    }
}

example.com {
    import logging
    tls admin@example.com

    handle_path /api/* {
        reverse_proxy backend:3000
    }

    handle /static/* {
        file_server
    }

    @websockets {
        header Connection *Upgrade*
    }

    respond "Not Found" 404
}

localhost:8080 {
    respond "Health OK" 200
}
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Snippet
        assert!(names.contains(&"(logging)"));

        // Site blocks
        assert!(names.contains(&"example.com"));
        assert!(names.contains(&"localhost:8080"));

        // Directives inside example.com
        assert!(
            names
                .iter()
                .any(|n| n.contains("handle_path") && n.contains("/api/*"))
        );
        assert!(
            names
                .iter()
                .any(|n| n.contains("handle") && n.contains("/static/*"))
        );
        assert!(names.iter().any(|n| n.contains("reverse_proxy")));
        assert!(names.iter().any(|n| n.contains("tls")));
        assert!(names.contains(&"@websockets"));
        assert!(names.iter().any(|n| n.starts_with("respond")));

        // Imports: global `import sites/*` + block-level `import logging`
        assert_eq!(result.imports.len(), 2);
        assert!(result.imports.iter().any(|i| i.source == "sites/*"));
        assert!(result.imports.iter().any(|i| i.source == "logging"));
    }
}
