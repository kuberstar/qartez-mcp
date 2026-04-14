use std::fmt;
use std::hash::{Hash, Hasher};

/// Symbols shorter than this many lines do not receive a shape hash.
/// One-liners and trivial getters would otherwise flood clone groups
/// with false positives.
const SHAPE_HASH_MIN_LINES: u32 = 3;

/// Normalized source shorter than this byte count is too small to
/// produce meaningful clone matches (avoids degenerate hashes from
/// near-empty bodies like `{ }` or `pass`).
const SHAPE_HASH_MIN_NORMALIZED_BYTES: usize = 10;

#[derive(Debug, Clone)]
pub enum SymbolKind {
    Function,
    Class,
    Interface,
    Type,
    Enum,
    Variable,
    Method,
    Struct,
    Trait,
    Const,
    Resource,
    Data,
    Module,
    Output,
    Local,
    Provider,
    /// Struct field, enum variant, or similar "member of a parent type"
    /// symbol. Always paired with a `parent_idx` pointing at the enclosing
    /// struct so `qartez_outline` can group them visually.
    Field,
    /// Makefile target or similar build target.
    Target,
    /// Docker multi-stage build stage (`FROM ... AS builder`).
    Stage,
    /// CI pipeline job (GitHub Actions, GitLab CI).
    Job,
    /// Container service definition (docker-compose `services:`).
    Service,
    /// CI workflow (GitHub Actions `name:` / trigger definition).
    Workflow,
    /// Ansible task or handler.
    Task,
    /// Named network definition (docker-compose `networks:`).
    Network,
    /// Named volume definition (docker-compose `volumes:`).
    Volume,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::Enum => "enum",
            Self::Variable => "variable",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Const => "const",
            Self::Resource => "resource",
            Self::Data => "data",
            Self::Module => "module",
            Self::Output => "output",
            Self::Local => "local",
            Self::Provider => "provider",
            Self::Field => "field",
            Self::Target => "target",
            Self::Stage => "stage",
            Self::Job => "job",
            Self::Service => "service",
            Self::Workflow => "workflow",
            Self::Task => "task",
            Self::Network => "network",
            Self::Volume => "volume",
        }
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub is_exported: bool,
    /// True when this symbol sits inside a `impl Trait for Type` block or a
    /// macro invocation's token tree. `qartez_unused` should skip these — trait
    /// impl methods are called via dynamic dispatch, macro-generated items
    /// are invisible to file-level imports. Extractors set this eagerly so
    /// the server never has to re-walk the AST.
    pub unused_excluded: bool,
    /// Index of this symbol's parent in the emitted `symbols` vector
    /// (e.g. the `struct` a field belongs to). `None` for top-level items.
    /// Stored as an index so extractors can build the parent link without
    /// knowing the DB rowid, which only exists after insertion.
    pub parent_idx: Option<usize>,
    /// Cyclomatic complexity of this symbol. Only meaningful for functions
    /// and methods — `None` for types, fields, and declarative constructs.
    /// Starts at 1 (one linear path) and increments for each branching
    /// point (if, match arm, loop, &&, ||, catch).
    pub complexity: Option<u32>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ExtractedImport {
    pub source: String,
    pub specifiers: Vec<String>,
    pub is_reexport: bool,
}

/// How an identifier in source code refers to another symbol. Used to tag
/// symbol-level edges so the resolver can weight a `Call` differently from a
/// plain type reference later if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    /// A call expression such as `foo()` or `self.bar(x)`.
    Call,
    /// A plain identifier use (variable read, path expression, field access).
    Use,
    /// A type position: `let x: Foo`, `fn f() -> Bar`, `impl Baz for _`.
    TypeRef,
}

impl ReferenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Use => "use",
            Self::TypeRef => "type",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtractedReference {
    /// Identifier the referring site used. Resolution later matches this
    /// against `symbols.name` with a same-file → imported-file → global
    /// fallback priority.
    pub name: String,
    /// 1-based line where the reference appears, used only for diagnostics.
    pub line: u32,
    /// Index of the enclosing symbol within `ParseResult::symbols` (for
    /// example, the function whose body contains this call). `None` when the
    /// reference lives at module scope. Later resolved to a real `symbols.id`
    /// via the insert-order id map that `insert_symbols` already produces.
    pub from_symbol_idx: Option<usize>,
    pub kind: ReferenceKind,
}

#[derive(Debug, Default)]
pub struct ParseResult {
    pub symbols: Vec<ExtractedSymbol>,
    pub imports: Vec<ExtractedImport>,
    pub references: Vec<ExtractedReference>,
}

/// Compute a structural fingerprint for a symbol's source code.
///
/// Normalizes the source text by stripping comments, replacing identifiers
/// with `_`, string literals with `_S`, and number literals with `_N`, then
/// collapsing whitespace. Two functions with identical control-flow structure
/// but different names/values produce the same hash (Type-I and Type-II clones
/// in the clone-detection taxonomy).
///
/// Returns `None` for symbols shorter than [`SHAPE_HASH_MIN_LINES`] or whose
/// normalized form is too short to be meaningful.
pub fn compute_shape_hash(source: &[u8], line_start: u32, line_end: u32) -> Option<String> {
    let body_lines = line_end.saturating_sub(line_start) + 1;
    if body_lines < SHAPE_HASH_MIN_LINES {
        return None;
    }

    let text = std::str::from_utf8(source).ok()?;
    let source_lines: Vec<&str> = text.lines().collect();

    let start = (line_start as usize).saturating_sub(1);
    let end = (line_end as usize).min(source_lines.len());
    if start >= source_lines.len() || start >= end {
        return None;
    }

    let snippet = source_lines[start..end].join("\n");
    let normalized = normalize_source(&snippet);

    if normalized.len() < SHAPE_HASH_MIN_NORMALIZED_BYTES {
        return None;
    }

    let mut hasher = std::hash::DefaultHasher::new();
    normalized.hash(&mut hasher);
    Some(format!("{:016x}", hasher.finish()))
}

/// Normalize source text into a structural skeleton suitable for hashing.
///
/// Strips `//` and `/* */` comments, replaces string/char/template literals,
/// numeric literals, and identifiers with fixed placeholders, then collapses
/// all whitespace runs into a single space. The result preserves only the
/// structural tokens (braces, operators, keywords-as-placeholders) so that
/// two code fragments with identical logic but different naming hash equally.
fn normalize_source(src: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;

    static RE_BLOCK_COMMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/\*[\s\S]*?\*/").unwrap());
    static RE_LINE_COMMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"//[^\n]*").unwrap());
    static RE_STRING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#""([^"\\]|\\.)*"|'([^'\\]|\\.)*'|`([^`\\]|\\.)*`"#).unwrap()
    });
    static RE_NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b\d[\d_.]*[a-zA-Z]*\b").unwrap());
    static RE_IDENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[a-zA-Z_]\w*\b").unwrap());
    static RE_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

    let s = RE_BLOCK_COMMENT.replace_all(src, "");
    let s = RE_LINE_COMMENT.replace_all(&s, "");
    let s = RE_STRING.replace_all(&s, "_S");
    let s = RE_NUMBER.replace_all(&s, "_N");
    let s = RE_IDENT.replace_all(&s, "_");
    let s = RE_WS.replace_all(&s, " ");
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_structure_same_hash() {
        let src_a = b"fn foo(x: i32) -> i32 {\n    if x > 0 {\n        return x + 1;\n    }\n    x - 1\n}\n";
        let src_b = b"fn bar(y: i32) -> i32 {\n    if y > 0 {\n        return y + 1;\n    }\n    y - 1\n}\n";
        let hash_a = compute_shape_hash(src_a, 1, 6).unwrap();
        let hash_b = compute_shape_hash(src_b, 1, 6).unwrap();
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn different_structure_different_hash() {
        let src_a = b"fn foo(x: i32) -> i32 {\n    if x > 0 {\n        return x + 1;\n    }\n    x - 1\n}\n";
        let src_b = b"fn bar(items: Vec<i32>) -> i32 {\n    let mut sum = 0;\n    for item in items {\n        sum += item;\n    }\n    sum\n}\n";
        let hash_a = compute_shape_hash(src_a, 1, 6).unwrap();
        let hash_b = compute_shape_hash(src_b, 1, 7).unwrap();
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn too_short_returns_none() {
        let src = b"fn f() { }\n";
        assert!(compute_shape_hash(src, 1, 1).is_none());
    }

    #[test]
    fn different_literals_same_hash() {
        let src_a = b"fn a() {\n    let x = \"hello\";\n    let y = 42;\n    println(x, y);\n}\n";
        let src_b = b"fn b() {\n    let x = \"world\";\n    let y = 99;\n    println(x, y);\n}\n";
        let hash_a = compute_shape_hash(src_a, 1, 5).unwrap();
        let hash_b = compute_shape_hash(src_b, 1, 5).unwrap();
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn comments_ignored() {
        let src_a = b"fn a() {\n    // do the thing\n    let x = 1;\n    x + 1\n}\n";
        let src_b = b"fn b() {\n    // something else entirely\n    let y = 2;\n    y + 2\n}\n";
        let hash_a = compute_shape_hash(src_a, 1, 5).unwrap();
        let hash_b = compute_shape_hash(src_b, 1, 5).unwrap();
        assert_eq!(hash_a, hash_b);
    }
}
