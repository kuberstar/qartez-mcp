#[allow(dead_code)]
#[derive(Clone)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub mtime_ns: i64,
    pub size_bytes: i64,
    pub language: String,
    pub line_count: i64,
    pub pagerank: f64,
    pub indexed_at: i64,
    /// Number of git commits that touched this file within the analysis
    /// window. Zero when git history is unavailable or the file is new.
    pub change_count: i64,
}

#[allow(dead_code)]
pub struct SymbolRow {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub is_exported: bool,
    pub shape_hash: Option<String>,
    pub parent_id: Option<i64>,
    /// Symbol-level PageRank computed over the `symbol_refs` graph. Zero when
    /// the symbol is unreferenced, an outlier (external-crate name), or the
    /// language has no reference extractor yet. Populated by
    /// `graph::pagerank::compute_symbol_pagerank` at the end of indexing, so
    /// every reader must select this column or fall back to zero.
    pub pagerank: f64,
    /// Cyclomatic complexity. `None` for non-function symbols or languages
    /// without control-flow extraction.
    pub complexity: Option<u32>,
    /// The type this method belongs to (e.g. "Foo" for `impl Foo { fn bar() }`).
    /// `None` for free functions and top-level items.
    pub owner_type: Option<String>,
}

/// Row in the `symbol_refs` table: one edge in the call / type / use graph
/// between two symbols. Populated by the resolution pass in `full_index`
/// after all symbols have been written.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SymbolRefRow {
    pub id: i64,
    pub from_symbol_id: i64,
    pub to_symbol_id: i64,
    /// Either "call", "use", or "type" — matches `ReferenceKind::as_str`.
    pub kind: String,
}

#[derive(Default)]
pub struct SymbolInsert {
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub is_exported: bool,
    pub shape_hash: Option<String>,
    /// True when this symbol sits inside a `impl Trait for Type` block or
    /// `macro_invocation` span. These symbols should never be reported as
    /// "unused" — trait-impl methods are called dynamically, macro-generated
    /// items are hidden from file-level imports. Pre-computed here so
    /// `qartez_unused` does not re-walk the tree at query time.
    pub unused_excluded: bool,
    /// `SymbolInsert`-local id of the parent symbol (e.g. the struct a field
    /// belongs to), or `None` for top-level items. Resolved to a real
    /// `symbols.id` at insert time.
    pub parent_idx: Option<usize>,
    /// Cyclomatic complexity for functions/methods.
    pub complexity: Option<u32>,
    /// The type this method belongs to (from `impl Foo { fn bar() }`).
    pub owner_type: Option<String>,
}

#[allow(dead_code)]
pub struct EdgeRow {
    pub id: i64,
    pub from_file: i64,
    pub to_file: i64,
    pub kind: String,
    pub specifier: Option<String>,
}

#[allow(dead_code)]
pub struct CoChangeRow {
    pub file_a: i64,
    pub file_b: i64,
    pub count: i64,
}

/// Row in the `type_hierarchy` table: one type relationship edge
/// (e.g. `impl Display for Foo`, `class Bar extends Baz`).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TypeHierarchyRow {
    pub id: i64,
    pub file_id: i64,
    pub sub_name: String,
    pub super_name: String,
    pub kind: String,
    pub line: u32,
}
