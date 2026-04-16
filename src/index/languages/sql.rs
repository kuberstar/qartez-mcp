// Rust guideline compliant 2026-04-13

use regex::Regex;
use std::sync::LazyLock;

use tree_sitter::Language;

use super::LanguageSupport;
use crate::index::symbols::{ExtractedSymbol, ParseResult, SymbolKind};

/// Regex-based SQL parser for `.sql` migration and schema files.
///
/// tree-sitter-sql is incompatible with tree-sitter 0.26, so this parser
/// uses line-oriented regex extraction following the same pattern as the
/// Dockerfile parser.
pub struct SqlSupport;

// CREATE TABLE [IF NOT EXISTS] [schema.]name
static CREATE_TABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)")
        .unwrap()
});

// CREATE INDEX [IF NOT EXISTS] name ON table
static CREATE_INDEX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)",
    )
    .unwrap()
});

// ALTER TABLE [schema.]name
static ALTER_TABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*ALTER\s+TABLE\s+(?:IF\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)").unwrap()
});

// CREATE [OR REPLACE] VIEW [IF NOT EXISTS] [schema.]name
static CREATE_VIEW_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(?:MATERIALIZED\s+)?VIEW\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)").unwrap()
});

// CREATE [OR REPLACE] FUNCTION [schema.]name
static CREATE_FUNCTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+([A-Za-z_][A-Za-z0-9_.]*)")
        .unwrap()
});

// CREATE [OR REPLACE] PROCEDURE [schema.]name
static CREATE_PROCEDURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?PROCEDURE\s+([A-Za-z_][A-Za-z0-9_.]*)")
        .unwrap()
});

// CREATE TRIGGER [IF NOT EXISTS] name
static CREATE_TRIGGER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?TRIGGER\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)",
    )
    .unwrap()
});

// CREATE TYPE [schema.]name
static CREATE_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+TYPE\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)")
        .unwrap()
});

// CREATE SCHEMA [IF NOT EXISTS] name
static CREATE_SCHEMA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+SCHEMA\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)")
        .unwrap()
});

// CREATE SEQUENCE [IF NOT EXISTS] [schema.]name
static CREATE_SEQUENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*CREATE\s+SEQUENCE\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)")
        .unwrap()
});

// END; or END $$ - terminates BEGIN...END blocks
static END_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*END\s*[;$]").unwrap());

/// Truncates a string to at most `max_len` characters.
fn truncate_signature(line: &str, max_len: usize) -> String {
    if line.len() <= max_len {
        line.to_string()
    } else {
        format!("{}...", &line[..max_len])
    }
}

impl LanguageSupport for SqlSupport {
    fn extensions(&self) -> &[&str] {
        &["sql"]
    }

    fn language_name(&self) -> &str {
        "sql"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        // SQL uses regex-based parsing; return YAML as a no-op
        // language so the tree-sitter machinery doesn't error.
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], _tree: &tree_sitter::Tree) -> ParseResult {
        let text = std::str::from_utf8(source).unwrap_or("");
        let mut symbols: Vec<ExtractedSymbol> = Vec::new();
        let mut seen_alter_tables: Vec<String> = Vec::new();

        // Track symbols that opened a BEGIN block so we can update their line_end.
        // Stores the index into `symbols` and whether the block is currently open.
        let mut open_block_idx: Option<usize> = None;

        let joined = text.replace("\\\n", " ");

        for (line_idx, line) in joined.lines().enumerate() {
            let trimmed = line.trim();
            let line_num = line_idx as u32 + 1;

            // Check for END of a BEGIN...END block
            if open_block_idx.is_some() && END_BLOCK_RE.is_match(trimmed) {
                if let Some(idx) = open_block_idx.take() {
                    symbols[idx].line_end = line_num;
                }
                continue;
            }

            // Detect BEGIN opening a block body
            if open_block_idx.is_some() {
                continue;
            }

            if let Some(cap) = CREATE_TABLE_RE.captures(trimmed) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Class,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = CREATE_INDEX_RE.captures(trimmed) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: false,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = CREATE_VIEW_RE.captures(trimmed) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Class,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = CREATE_FUNCTION_RE.captures(trimmed) {
                let idx = symbols.len();
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
                // Functions may have BEGIN...END blocks
                if trimmed.to_uppercase().contains("BEGIN") {
                    open_block_idx = Some(idx);
                }
            } else if let Some(cap) = CREATE_PROCEDURE_RE.captures(trimmed) {
                let idx = symbols.len();
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
                if trimmed.to_uppercase().contains("BEGIN") {
                    open_block_idx = Some(idx);
                }
            } else if let Some(cap) = CREATE_TRIGGER_RE.captures(trimmed) {
                let idx = symbols.len();
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Function,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
                if trimmed.to_uppercase().contains("BEGIN") {
                    open_block_idx = Some(idx);
                }
            } else if let Some(cap) = CREATE_TYPE_RE.captures(trimmed) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Type,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = CREATE_SCHEMA_RE.captures(trimmed) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Module,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = CREATE_SEQUENCE_RE.captures(trimmed) {
                symbols.push(ExtractedSymbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Variable,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(truncate_signature(trimmed, 200)),
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                    owner_type: None,
                });
            } else if let Some(cap) = ALTER_TABLE_RE.captures(trimmed) {
                let name = cap[1].to_string();
                let lower = name.to_lowercase();
                if !seen_alter_tables.contains(&lower) {
                    seen_alter_tables.push(lower);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Class,
                        line_start: line_num,
                        line_end: line_num,
                        signature: Some(truncate_signature(trimmed, 200)),
                        is_exported: true,
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                        owner_type: None,
                    });
                }
            }

            // Detect a standalone BEGIN line that opens a block for the last symbol
            if (trimmed.eq_ignore_ascii_case("BEGIN") || trimmed.eq_ignore_ascii_case("BEGIN;"))
                && symbols
                    .last()
                    .is_some_and(|s| matches!(s.kind, SymbolKind::Function))
                && open_block_idx.is_none()
            {
                open_block_idx = Some(symbols.len() - 1);
            }
        }

        ParseResult {
            symbols,
            imports: Vec::new(),
            references: Vec::new(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_sql(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = SqlSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_create_table() {
        let result = parse_sql("CREATE TABLE users (\n  id SERIAL PRIMARY KEY\n);\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "users");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_create_index() {
        let result = parse_sql("CREATE UNIQUE INDEX idx_users_email ON users (email);\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "idx_users_email");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Variable));
        assert!(!result.symbols[0].is_exported);
    }

    #[test]
    fn test_create_view() {
        let result = parse_sql("CREATE VIEW active_users AS SELECT * FROM users WHERE active;\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "active_users");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_create_function() {
        let result = parse_sql(
            "CREATE FUNCTION get_user(uid INT) RETURNS TEXT AS $$\nBEGIN\n  RETURN 'ok';\nEND;\n$$ LANGUAGE plpgsql;\n",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "get_user");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_create_procedure() {
        let result = parse_sql(
            "CREATE PROCEDURE cleanup_old_data()\nLANGUAGE SQL\nAS $$ DELETE FROM logs WHERE created < NOW() - INTERVAL '90 days'; $$;\n",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "cleanup_old_data");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_create_trigger() {
        let result = parse_sql(
            "CREATE TRIGGER update_timestamp BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION set_updated_at();\n",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "update_timestamp");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_create_type() {
        let result = parse_sql("CREATE TYPE mood AS ENUM ('happy', 'sad', 'neutral');\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "mood");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Type));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_alter_table() {
        let result = parse_sql(
            "ALTER TABLE users ADD COLUMN email TEXT;\nALTER TABLE users ADD COLUMN name TEXT;\n",
        );
        assert_eq!(result.symbols.len(), 1, "only first ALTER TABLE occurrence");
        assert_eq!(result.symbols[0].name, "users");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_if_not_exists() {
        let result =
            parse_sql("CREATE TABLE IF NOT EXISTS sessions (\n  id UUID PRIMARY KEY\n);\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "sessions");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_empty_file() {
        let result = parse_sql("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
        assert!(result.references.is_empty());
    }

    #[test]
    fn test_mixed_migration() {
        let result = parse_sql(
            r#"-- Migration: 001_initial
CREATE SCHEMA IF NOT EXISTS app;
CREATE TYPE app.status AS ENUM ('active', 'inactive');
CREATE TABLE app.users (
  id SERIAL PRIMARY KEY,
  name TEXT NOT NULL,
  status app.status DEFAULT 'active'
);
CREATE INDEX idx_users_name ON app.users (name);
CREATE SEQUENCE app.order_seq START 1000;
CREATE VIEW app.active_users AS SELECT * FROM app.users WHERE status = 'active';
ALTER TABLE app.users ADD COLUMN email TEXT;
CREATE OR REPLACE FUNCTION app.get_user(uid INT) RETURNS TEXT AS $$
BEGIN
  RETURN 'ok';
END;
$$ LANGUAGE plpgsql;
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"app"));
        assert!(names.contains(&"app.status"));
        assert!(names.contains(&"app.users"));
        assert!(names.contains(&"idx_users_name"));
        assert!(names.contains(&"app.order_seq"));
        assert!(names.contains(&"app.active_users"));
        assert!(names.contains(&"app.get_user"));
        assert_eq!(result.symbols.len(), 8);
    }

    #[test]
    fn test_schema_qualified_name() {
        let result = parse_sql("CREATE TABLE public.accounts (\n  id INT PRIMARY KEY\n);\n");
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "public.accounts");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_create_or_replace() {
        let result = parse_sql(
            "CREATE OR REPLACE FUNCTION update_ts() RETURNS TRIGGER AS $$\nBEGIN\n  NEW.updated_at = NOW();\n  RETURN NEW;\nEND;\n$$ LANGUAGE plpgsql;\n",
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "update_ts");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Function));
        assert!(result.symbols[0].is_exported);
    }
}
