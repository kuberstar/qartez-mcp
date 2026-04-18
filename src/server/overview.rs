// Rust guideline compliant 2026-04-15

//! Codebase overview renderers for `qartez_map` and the `qartez://overview`
//! resource. Produces the ranked file / symbol table that most tool
//! interactions start from.

use std::collections::HashSet;

use super::helpers::{elide_file_source, estimate_tokens, truncate_path};
use crate::graph::blast;
use crate::storage::read;

impl super::QartezServer {
    pub(super) fn project_name(&self) -> String {
        self.project_root
            .canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .or_else(|| {
                self.project_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Render a symbol-centric overview: top symbols by symbol-level
    /// PageRank, grouped by their defining file. Invoked from `qartez_map`
    /// when the caller passes `by=symbols`. Designed to be a drop-in
    /// replacement for the file-ranked overview on the same token budget,
    /// so the output structure intentionally mirrors `build_overview`'s
    /// header/table shape.
    pub(super) fn build_symbol_overview(
        &self,
        top_n: i64,
        token_budget: usize,
        concise: bool,
    ) -> String {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => return format!("DB lock error: {e}"),
        };
        let file_count = read::get_file_count(&conn).unwrap_or(0);
        let symbol_count = read::get_symbol_count(&conn).unwrap_or(0);
        let effective_limit = if top_n == i64::MAX { 1000 } else { top_n };

        let symbols = match read::get_symbols_ranked(&conn, effective_limit) {
            Ok(s) => s,
            Err(e) => return format!("Error reading symbols: {e}"),
        };

        let any_nonzero = symbols.iter().any(|(s, _)| s.pagerank > 0.0);
        if !any_nonzero {
            let mut out = String::from(
                "# Symbol PageRank unavailable\n\
                 No symbol-level PageRank data in the index. This is expected for \
                 languages without a reference extractor yet (see docs) or DBs that \
                 predate symbol PageRank. Falling back to file ranking.\n\n",
            );
            out.push_str(&self.build_overview(top_n, token_budget, None, None, concise, false));
            return out;
        }

        let mut out = String::new();
        if concise {
            out.push_str(&format!(
                "{file_count} files, {symbol_count} symbols (rank name kind file PR)\n",
            ));
        } else {
            out.push_str(&format!(
                "# Codebase: {} ({} files, {} symbols indexed) - by symbols\n\n",
                self.project_name(),
                file_count,
                symbol_count,
            ));
            out.push_str(" # | Symbol                         | Kind       | File                               | PageRank\n");
            out.push_str("---+--------------------------------+------------+------------------------------------+---------\n");
        }

        for (i, (sym, file)) in symbols.iter().enumerate() {
            if sym.pagerank <= 0.0 {
                break;
            }
            let line = if concise {
                format!(
                    "{} {} {} {} {:.4}\n",
                    i + 1,
                    sym.name,
                    sym.kind,
                    file.path,
                    sym.pagerank,
                )
            } else {
                format!(
                    "{:>2} | {:<30} | {:<10} | {:<34} | {:>8.4}\n",
                    i + 1,
                    truncate_path(&sym.name, 30),
                    truncate_path(&sym.kind, 10),
                    truncate_path(&file.path, 34),
                    sym.pagerank,
                )
            };
            if estimate_tokens(&out) + estimate_tokens(&line) > token_budget {
                break;
            }
            out.push_str(&line);
        }

        out
    }

    pub(super) fn build_overview(
        &self,
        top_n: i64,
        token_budget: usize,
        boost_files: Option<&[String]>,
        boost_terms: Option<&[String]>,
        concise: bool,
        all_files: bool,
    ) -> String {
        let conn = match self.db.lock() {
            Ok(c) => c,
            Err(e) => return format!("DB lock error: {e}"),
        };
        let file_count = read::get_file_count(&conn).unwrap_or(0);
        let symbol_count = read::get_symbol_count(&conn).unwrap_or(0);

        let has_boosts = boost_files.is_some() || boost_terms.is_some();
        let fetch_limit = if has_boosts {
            top_n.saturating_mul(3)
        } else {
            top_n
        };

        let mut files = if all_files {
            match read::get_all_files_ranked(&conn) {
                Ok(f) => f,
                Err(e) => return format!("Error reading files: {e}"),
            }
        } else {
            match read::get_files_ranked(&conn, fetch_limit) {
                Ok(f) => f,
                Err(e) => return format!("Error reading files: {e}"),
            }
        };

        if !all_files && has_boosts {
            let mut boosted_ids: HashSet<i64> = HashSet::new();

            if let Some(paths) = boost_files {
                for file in &files {
                    for bp in paths {
                        if file.path.contains(bp.as_str()) {
                            boosted_ids.insert(file.id);
                        }
                    }
                }
            }

            if let Some(terms) = boost_terms {
                for term in terms {
                    let fts_query = if term.contains('*') {
                        term.clone()
                    } else {
                        format!("{term}*")
                    };
                    if let Ok(ids) = read::search_file_ids_by_fts(&conn, &fts_query) {
                        for id in ids {
                            boosted_ids.insert(id);
                        }
                    }
                }
            }

            if !boosted_ids.is_empty() {
                for file in &mut files {
                    if boosted_ids.contains(&file.id) {
                        file.pagerank *= 10.0;
                    }
                }
                files.sort_by(|a, b| {
                    b.pagerank
                        .partial_cmp(&a.pagerank)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            files.truncate(top_n as usize);
        }

        let visible_ids: Vec<i64> = files.iter().map(|f| f.id).collect();
        let blast_radii = blast::blast_radius_for_files(&conn, &visible_ids).unwrap_or_default();

        let mut out = String::new();
        if concise {
            out.push_str(&format!(
                "{file_count} files, {symbol_count} symbols (rank path PR exp \u{2192}blast)\n",
            ));
        } else {
            out.push_str(&format!(
                "# Codebase: {} ({} files, {} symbols indexed)\n\n",
                self.project_name(),
                file_count,
                symbol_count,
            ));
            out.push_str(" # | File                                | PageRank | Exports | Blast\n");
            out.push_str("---+-------------------------------------+----------+---------+------\n");
        }

        let mut file_symbols: Vec<(String, Vec<crate::storage::models::SymbolRow>)> = Vec::new();

        for (i, file) in files.iter().enumerate() {
            let symbols = read::get_symbols_for_file(&conn, file.id).unwrap_or_default();
            let export_count = symbols.iter().filter(|s| s.is_exported).count();
            let blast_r = blast_radii.get(&file.id).copied().unwrap_or(0);

            let line = if concise {
                format!(
                    "{} {} {:.3} {} \u{2192}{}\n",
                    i + 1,
                    file.path,
                    file.pagerank,
                    export_count,
                    blast_r,
                )
            } else {
                format!(
                    "{:>2} | {:<35} | {:>8.4} | {:>7} | \u{2192}{}\n",
                    i + 1,
                    truncate_path(&file.path, 35),
                    file.pagerank,
                    export_count,
                    blast_r,
                )
            };

            if estimate_tokens(&out) + estimate_tokens(&line) > token_budget {
                break;
            }
            out.push_str(&line);
            file_symbols.push((file.path.clone(), symbols));
        }

        if !concise {
            out.push('\n');
        }

        if !concise {
            let roots = self.project_roots.read().unwrap();
            let aliases = self.root_aliases.read().unwrap();
            for (path, symbols) in &file_symbols {
                let exported: Vec<&crate::storage::models::SymbolRow> =
                    symbols.iter().filter(|s| s.is_exported).collect();
                if exported.is_empty() {
                    continue;
                }

                let section_header = format!("## {path}\n");
                if estimate_tokens(&out) + estimate_tokens(&section_header) > token_budget {
                    break;
                }
                out.push_str(&section_header);

                let remaining = token_budget.saturating_sub(estimate_tokens(&out));

                if let Some(elided) = elide_file_source(
                    &self.project_root,
                    &roots,
                    &aliases,
                    path,
                    symbols,
                    remaining,
                ) {
                    out.push_str(&elided);
                } else {
                    for sym in &exported {
                        let fallback = format!("{} {}", sym.kind, sym.name);
                        let sig = sym.signature.as_deref().unwrap_or(&fallback);
                        let line = format!("  + {sig}\n");
                        if estimate_tokens(&out) + estimate_tokens(&line) > token_budget {
                            break;
                        }
                        out.push_str(&line);
                    }
                }
                out.push('\n');
            }
        }

        out
    }
}
