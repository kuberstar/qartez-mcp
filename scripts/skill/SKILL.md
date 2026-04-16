---
name: qartez
description: >-
  Orchestrate Qartez code intelligence MCP tools for code exploration, architecture
  analysis, refactoring, debugging, code review, impact analysis, and onboarding.
  Use this skill whenever performing multi-step code exploration with qartez tools,
  reviewing files for blast radius, tracing call hierarchies, finding dead code,
  detecting duplicates, checking architecture boundaries, or planning refactors.
  Also trigger on: 'review this file', 'find dead code', 'what calls this function',
  'show architecture', 'check boundaries', 'find duplicates', 'impact analysis',
  'rename symbol', 'move function', 'onboard me', 'hotspots', 'code review',
  'dependency graph', 'blast radius', architecture overview, debugging call chains,
  or when about to modify a load-bearing file. Even if the user does not mention
  qartez explicitly, trigger when the task involves semantic code analysis beyond
  simple text search.
allowed-tools:
  - mcp__qartez__qartez_map
  - mcp__qartez__qartez_find
  - mcp__qartez__qartez_grep
  - mcp__qartez__qartez_read
  - mcp__qartez__qartez_outline
  - mcp__qartez__qartez_stats
  - mcp__qartez__qartez_impact
  - mcp__qartez__qartez_deps
  - mcp__qartez__qartez_refs
  - mcp__qartez__qartez_calls
  - mcp__qartez__qartez_cochange
  - mcp__qartez__qartez_context
  - mcp__qartez__qartez_unused
  - mcp__qartez__qartez_hotspots
  - mcp__qartez__qartez_clones
  - mcp__qartez__qartez_boundaries
  - mcp__qartez__qartez_rename
  - mcp__qartez__qartez_move
  - mcp__qartez__qartez_rename_file
  - mcp__qartez__qartez_project
  - mcp__qartez__qartez_wiki
  - mcp__qartez__qartez_diff_impact
  - mcp__qartez__qartez_trend
  - mcp__qartez__qartez_hierarchy
---

# Qartez Code Intelligence Skill

## Overview

Qartez is an AST-based code intelligence MCP server that provides semantic understanding of codebases. It maintains a pre-computed index of symbols, dependencies, call graphs, and git history, making queries fast and token-efficient.

Use Qartez tools instead of built-in file tools for all code exploration. The index operates on parsed AST nodes rather than raw text, so results are structurally accurate: a symbol search returns definitions, not string matches in comments or variable names that happen to contain the substring.

## Tool Mapping

Replace built-in file tools with Qartez equivalents:

| Instead of | Use | When |
|---|---|---|
| Glob / find | `qartez_map` | Understanding project structure, finding files |
| Grep / rg | `qartez_grep` | Searching for symbols, types, functions |
| Grep / rg | `qartez_find` | Looking up a specific symbol definition |
| Read / cat | `qartez_read` | Reading symbol source code with context |

Beyond these four replacements, Qartez provides 20+ additional tools for analysis, risk assessment, and refactoring. See `references/tools.md` for the complete reference.

## Workflows by Task Type

Each workflow below lists tools in the order you should call them and explains why each step matters.

### Explore a New Codebase

Goal: Build a mental model of the project before making any changes.

1. **`qartez_map`** - Get the project skeleton. This shows directory structure ranked by importance (PageRank), so you immediately know which files are central.
2. **`qartez_stats`** - Understand the scale: languages used, lines of code, symbol counts. This sets expectations for complexity.
3. **`qartez_outline`** on 2-3 top-ranked files - Read the symbol tables of key files without loading full source. This reveals the project's core abstractions.
4. **`qartez_wiki`** - Generate an architecture overview via community detection. This groups files into logical modules and explains how they relate.

### Find and Understand a Symbol

Goal: Locate a symbol definition and understand how it fits into the codebase.

1. **`qartez_find`** with the symbol name - Jump directly to the definition. Unlike text grep, this resolves to the AST node, giving you the exact file and line.
2. **`qartez_read`** - Read the source with semantic context. This includes surrounding code that helps you understand the symbol's role.
3. **`qartez_refs`** - Find every usage across the codebase. This tells you how widely the symbol is consumed and in what patterns.
4. **`qartez_calls`** - Trace the call hierarchy. See what the symbol calls (callees) and what calls it (callers) to understand its position in execution flow.

### Review a File Before Editing

Goal: Understand a file's role, risk level, and coupling before making changes.

1. **`qartez_outline`** - Get the symbol table first. This is cheaper than reading full source and shows you the file's API surface.
2. **`qartez_impact`** - Check the blast radius. This reveals how many files depend on this file transitively, so you know the scope of potential breakage.
3. **`qartez_cochange`** - See which files historically change together with this one. If you modify this file, you likely need to update its co-change partners too.
4. **`qartez_deps`** - View the dependency edges (imports and exports). This shows the file's direct connections to the rest of the codebase.

### Debug a Bug

Goal: Trace execution flow to find where a bug originates.

1. **`qartez_find`** with the relevant symbol (error message, function name, type) - Locate the starting point.
2. **`qartez_read`** - Read the implementation to understand the current logic.
3. **`qartez_calls`** - Trace the call chain. Walk callers upward to find what triggers the buggy path, or walk callees downward to find where the logic goes wrong.
4. **`qartez_refs`** - Check all usages to see if other call sites handle edge cases differently, which can reveal the intended behavior.

### Refactor Safely

Goal: Restructure code without breaking dependents.

1. **`qartez_impact`** - Start by measuring the blast radius. If the target symbol or file has high PageRank or many transitive dependents, proceed with extra caution.
2. **`qartez_context`** - Surface all files related to the planned change. This catches files you might otherwise miss that reference the target indirectly.
3. **`qartez_hotspots`** - Identify the highest-risk areas (complexity x coupling x churn). Focus refactoring effort where it will have the most impact.
4. **`qartez_clones`** - Detect duplicate code via AST hashing. If the code you plan to refactor has clones elsewhere, consider consolidating them in the same pass.
5. **`qartez_rename`** / **`qartez_move`** - Execute the refactoring. These tools update all references across the codebase automatically.

### Architecture Overview

Goal: Produce a high-level understanding of the system's structure and health.

1. **`qartez_map`** - Get the ranked file tree. High-PageRank files form the architectural backbone.
2. **`qartez_stats`** - Quantify the project: languages, size, symbol density.
3. **`qartez_hotspots`** - Find where risk concentrates. Files with high complexity, high coupling, and frequent churn are architectural pain points.
4. **`qartez_wiki`** - Generate a full architecture document using community detection to identify logical modules.
5. **`qartez_boundaries`** - Check whether the codebase respects its declared architecture rules (if `.qartez/boundaries.toml` exists).

### Find Dead Code

Goal: Identify unused exports that can be safely removed.

1. **`qartez_unused`** - Scan for exported symbols with zero internal consumers. This is the primary dead-code detection tool.
2. **`qartez_refs`** on each candidate - Verify that the symbol truly has zero usages. Some symbols may be used dynamically or via external consumers that the index does not track.

### Pre-merge Check

Goal: Assess the risk of a set of changes before merging.

1. **`qartez_diff_impact`** - Analyze the current diff to see which files and symbols are affected and their blast radii.
2. **`qartez_context`** - Surface related files that the diff might have missed. If context reveals untouched files that should have been updated, flag them.
3. **`qartez_boundaries`** - Verify the changes do not violate architecture boundary rules.

## Modification Guard

A PreToolUse hook (`qartez-guard`) blocks Edit, Write, and MultiEdit on load-bearing files, those with PageRank >= 0.05 or transitive blast radius >= 10. When blocked, the error message tells you exactly which thresholds fired.

To acknowledge the risk and proceed: call `qartez_impact` with `file_path=<the file>`. This grants edit access for 10 minutes.

For configuration details, environment variable overrides, and how to disable the guard, see `references/guard.md`.

## Reference Files

- **`references/tools.md`** - Complete reference for all 24 tools with parameters, return values, and trigger phrases
- **`references/guard.md`** - Modification guard configuration, thresholds, and environment variable overrides
