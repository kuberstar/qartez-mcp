//! REST endpoints under `/api/*`. Browser-facing JSON only.

pub mod clones;
pub mod dead_code;
pub mod focused_file;
pub mod focused_symbol;
pub mod graph;
pub mod graph_diff;
pub mod health;
pub mod hotspots;
pub mod project;
pub mod project_health;
pub mod reindex;
pub mod shutdown;
pub mod smells;
pub mod symbol_cochanges;
pub mod symbol_graph;
pub mod symbol_search;
