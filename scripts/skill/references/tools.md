# Qartez Tools Reference

Complete reference for all Qartez MCP tools, grouped by category.

---

## Navigate

Tools for finding files, symbols, and understanding project structure.

### qartez_map

Project skeleton view ranked by importance.

- **When to use**: Starting a new exploration, finding files, understanding project layout.
- **Trigger phrases**: "show project structure", "what files are in this project", "where is X located"
- **Parameters**:
  - `path` (optional) - Subdirectory to scope the map to
  - `depth` (optional) - Maximum directory depth to display
- **Returns**: Tree of files and directories with PageRank scores. Higher-ranked files are more central to the codebase.

### qartez_find

Jump directly to a symbol definition.

- **When to use**: You know the symbol name and want its definition location.
- **Trigger phrases**: "find definition of X", "where is X defined", "jump to X"
- **Parameters**:
  - `symbol` - Name of the symbol to find (function, type, constant, etc.)
  - `kind` (optional) - Filter by symbol kind (function, struct, type, const, etc.)
- **Returns**: File path, line number, and symbol kind for matching definitions.

### qartez_grep

Search symbols across the codebase by pattern.

- **When to use**: Searching for symbols when you only know part of the name, or searching for a pattern across many files.
- **Trigger phrases**: "search for X", "find all symbols matching X", "grep for X"
- **Parameters**:
  - `pattern` - Search pattern (supports regex)
  - `kind` (optional) - Filter by symbol kind
  - `path` (optional) - Scope search to a subdirectory
- **Returns**: List of matching symbols with file paths and line numbers.

### qartez_read

Read symbol source code with semantic context.

- **When to use**: Reading a specific symbol's implementation along with its surrounding context (imports, related types).
- **Trigger phrases**: "show me the code for X", "read X", "what does X do"
- **Parameters**:
  - `symbol` - Symbol name to read
  - `file_path` (optional) - Disambiguate when multiple files define the same symbol
  - `context_lines` (optional) - Number of surrounding lines to include
- **Returns**: Source code of the symbol with relevant context lines.

### qartez_outline

File symbol table (names, kinds, line numbers).

- **When to use**: Getting a quick overview of a file's contents without reading full source. Cheaper than qartez_read for initial exploration.
- **Trigger phrases**: "what's in this file", "outline of X", "list functions in X"
- **Parameters**:
  - `file_path` - Path to the file
- **Returns**: List of symbols with their kinds (function, struct, const, etc.) and line numbers.

### qartez_stats

Language, LOC, and symbol count breakdown.

- **When to use**: Understanding project scale and composition. Works at project or per-file level.
- **Trigger phrases**: "how big is this project", "what languages", "project stats", "lines of code"
- **Parameters**:
  - `path` (optional) - Scope to a subdirectory or specific file
- **Returns**: Breakdown by language with lines of code, file count, and symbol count.

---

## Analyze

Tools for understanding dependencies, usage patterns, and relationships.

### qartez_impact

Blast radius analysis for a file or symbol.

- **When to use**: Before modifying any heavily-imported file or exported symbol. Required by the modification guard for load-bearing files.
- **Trigger phrases**: "blast radius of X", "what depends on X", "impact analysis", "is it safe to change X"
- **Parameters**:
  - `file_path` - Path to the file to analyze
  - `symbol` (optional) - Specific symbol within the file
- **Returns**: PageRank score, direct dependents count, transitive blast radius, and list of affected files.

### qartez_deps

File dependency graph (imports and exports).

- **When to use**: Understanding what a file depends on and what depends on it. Useful for visualizing module boundaries.
- **Trigger phrases**: "what does X import", "dependency graph", "what depends on X"
- **Parameters**:
  - `file_path` - Path to the file
  - `direction` (optional) - "imports", "exports", or "both" (default: "both")
- **Returns**: Lists of imported and exported symbols with their source/target files.

### qartez_refs

All references to a symbol across the codebase.

- **When to use**: Finding every usage of a symbol. Essential before renaming, removing, or changing a symbol's signature.
- **Trigger phrases**: "who uses X", "find all references to X", "usages of X"
- **Parameters**:
  - `symbol` - Symbol name to find references for
  - `file_path` (optional) - Disambiguate the symbol's definition file
- **Returns**: List of files and lines where the symbol is referenced.

### qartez_calls

Call hierarchy: callers and callees of a function.

- **When to use**: Tracing execution flow during debugging. Understanding how a function fits into the call graph.
- **Trigger phrases**: "what calls X", "what does X call", "call hierarchy", "trace calls"
- **Parameters**:
  - `symbol` - Function name
  - `direction` (optional) - "callers", "callees", or "both" (default: "both")
  - `depth` (optional) - How many levels deep to trace
- **Returns**: Tree of callers and/or callees with file paths and line numbers.

### qartez_cochange

Files that historically change together in git history.

- **When to use**: Before editing a file, to discover other files you likely need to update. Based on git commit co-occurrence patterns.
- **Trigger phrases**: "what changes with X", "co-change partners", "related changes"
- **Parameters**:
  - `file_path` - Path to the file
  - `min_confidence` (optional) - Minimum co-change confidence threshold
- **Returns**: Ranked list of files that frequently change alongside the target, with confidence scores.

### qartez_context

Related files for a set of files you plan to modify.

- **When to use**: Before starting a multi-file change. Surfaces files you might miss that are related via dependencies, co-change, or shared symbols.
- **Trigger phrases**: "what else should I change", "related files", "context for this change"
- **Parameters**:
  - `file_paths` - List of files you plan to modify
- **Returns**: Ranked list of related files with reasons why each is relevant (dependency, co-change, shared symbol, etc.).

### qartez_unused

Find dead/unused exported symbols.

- **When to use**: Cleaning up dead code. Identifying exports that no other file in the project consumes.
- **Trigger phrases**: "find dead code", "unused exports", "what can I delete"
- **Parameters**:
  - `path` (optional) - Scope to a subdirectory
  - `kind` (optional) - Filter by symbol kind
- **Returns**: List of exported symbols with zero internal references.

---

## Risk

Tools for identifying high-risk code and enforcing architectural rules.

### qartez_hotspots

High-risk code: complexity x coupling x churn.

- **When to use**: Prioritizing refactoring effort. Files with high scores across all three dimensions are the most likely sources of bugs and the best candidates for improvement.
- **Trigger phrases**: "hotspots", "risky code", "what should I refactor", "technical debt"
- **Parameters**:
  - `path` (optional) - Scope to a subdirectory
  - `top_n` (optional) - Number of results to return
- **Returns**: Ranked list of files with composite hotspot scores and individual metrics for complexity, coupling, and churn.

### qartez_clones

Detect duplicate code via AST structural hashing.

- **When to use**: Finding refactoring opportunities where similar logic has been copy-pasted. Unlike text-based duplicate detection, this finds structurally identical code even when variable names differ.
- **Trigger phrases**: "find duplicates", "copy-paste detection", "clone detection", "DRY violations"
- **Parameters**:
  - `path` (optional) - Scope to a subdirectory
  - `min_tokens` (optional) - Minimum clone size in AST tokens
- **Returns**: Groups of structurally identical code blocks with file paths and line ranges.

### qartez_boundaries

Check architecture boundary rules defined in `.qartez/boundaries.toml`.

- **When to use**: Verifying that code changes respect declared module boundaries. Run this before merging to catch architectural violations.
- **Trigger phrases**: "check boundaries", "architecture rules", "module violations", "boundary check"
- **Parameters**:
  - `path` (optional) - Scope to specific files or directories
- **Returns**: List of boundary violations (if any) with the rule that was broken and the offending import.

---

## Refactor

Tools that modify code across the codebase.

### qartez_rename

Rename a symbol across the entire codebase.

- **When to use**: Renaming a function, type, constant, or other symbol. Updates all references automatically.
- **Trigger phrases**: "rename X to Y", "change name of X"
- **Parameters**:
  - `symbol` - Current symbol name
  - `new_name` - New name for the symbol
  - `file_path` (optional) - Disambiguate the symbol's definition file
- **Returns**: List of files modified and the number of references updated.

### qartez_move

Move a symbol from one file to another.

- **When to use**: Reorganizing code by moving a function, type, or constant to a different file. Updates all imports and references.
- **Trigger phrases**: "move X to Y", "extract X into Y", "relocate function"
- **Parameters**:
  - `symbol` - Symbol name to move
  - `from_file` - Source file path
  - `to_file` - Destination file path
- **Returns**: List of files modified with updated import paths.

### qartez_rename_file

Rename a file and update all imports that reference it.

- **When to use**: Renaming or relocating a file while keeping all imports valid.
- **Trigger phrases**: "rename file X to Y", "move file", "change file path"
- **Parameters**:
  - `old_path` - Current file path
  - `new_path` - New file path
- **Returns**: List of files with updated import paths.

---

## Build

Tools for project-level operations and documentation generation.

### qartez_project

Detected toolchain information: test, build, and lint commands.

- **When to use**: Understanding how to build, test, and lint a project. Detects package managers, build systems, and testing frameworks.
- **Trigger phrases**: "how do I build this", "what test framework", "project setup"
- **Parameters**: None
- **Returns**: Detected build tool, test runner, linter, and their respective commands.

### qartez_wiki

Generate an architecture document via community detection.

- **When to use**: Creating or updating architecture documentation. Groups files into logical modules based on dependency patterns and generates prose descriptions.
- **Trigger phrases**: "generate architecture doc", "write ARCHITECTURE.md", "document the architecture"
- **Parameters**:
  - `path` (optional) - Scope to a subdirectory
  - `detail` (optional) - Level of detail ("brief" or "full")
- **Returns**: Markdown architecture document with module descriptions, key files, and dependency relationships.

---

## Git-aware

Tools that incorporate git history for richer analysis.

### qartez_diff_impact

Analyze the current git diff for blast radius and risk.

- **When to use**: Before merging or committing. Evaluates the current set of changes for their impact on the rest of the codebase.
- **Trigger phrases**: "check my changes", "pre-merge check", "diff impact", "is this safe to merge"
- **Parameters**:
  - `base` (optional) - Base branch or commit to diff against (default: HEAD)
- **Returns**: Per-file impact scores for changed files, affected dependents, and overall risk assessment.

### qartez_trend

Historical trend analysis for metrics over git history.

- **When to use**: Understanding how code quality metrics have changed over time. Tracks complexity, coupling, and size trends.
- **Trigger phrases**: "show trends", "how has complexity changed", "metric history"
- **Parameters**:
  - `file_path` (optional) - Specific file to track
  - `metric` (optional) - Specific metric to track
  - `commits` (optional) - Number of historical commits to analyze
- **Returns**: Time series of metric values across recent commits.

### qartez_hierarchy

Module hierarchy and nesting structure.

- **When to use**: Understanding the logical module structure of the project, especially in languages with explicit module systems (Rust, Python, Go).
- **Trigger phrases**: "module hierarchy", "module tree", "package structure"
- **Parameters**:
  - `path` (optional) - Scope to a subdirectory
- **Returns**: Tree of modules/packages with their contained symbols and submodules.
