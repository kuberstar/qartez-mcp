use tree_sitter::{Language, Node};

use super::LanguageSupport;
use crate::index::symbols::{ExtractedImport, ExtractedSymbol, ParseResult, SymbolKind};

pub struct JenkinsfileSupport;

impl LanguageSupport for JenkinsfileSupport {
    fn extensions(&self) -> &[&str] {
        &["groovy", "jenkinsfile"]
    }

    fn filenames(&self) -> &[&str] {
        &["Jenkinsfile"]
    }

    fn filename_prefixes(&self) -> &[&str] {
        &["Jenkinsfile."]
    }

    fn language_name(&self) -> &str {
        "jenkinsfile"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_groovy::LANGUAGE)
    }

    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult {
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, &mut symbols, &mut imports);
        ParseResult {
            symbols,
            imports,
            references: Vec::new(),
        }
    }
}

fn children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.child_count() as u32).filter_map(move |i| node.child(i))
}

fn extract_from_node(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    imports: &mut Vec<ExtractedImport>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(sym) = extract_function(node, source) {
                symbols.push(sym);
            }
        }
        "class_declaration" => {
            if let Some(sym) = extract_class(node, source) {
                symbols.push(sym);
            }
        }
        "method_invocation" => {
            extract_dsl_block(node, source, symbols);
        }
        "local_variable_declaration" => {
            extract_local_variable(node, source, symbols);
        }
        "expression_statement" => {
            // Check for bare assignment expressions (BRANCH_NAME = "main")
            for child in children(node) {
                if child.kind() == "assignment_expression"
                    && let Some(sym) = extract_assignment(child, source) {
                        symbols.push(sym);
                    }
            }
        }
        "import_declaration" => {
            extract_import(node, source, imports);
        }
        _ => {}
    }

    for child in children(node) {
        extract_from_node(child, source, symbols, imports);
    }
}

/// Extracts `def functionName(args) { ... }` definitions.
fn extract_function(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|n| n.kind() == "identifier")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Function,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

/// Extracts `class Name { ... }` declarations.
fn extract_class(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let name = children(node)
        .find(|n| n.kind() == "identifier")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Class,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, source),
        is_exported: true,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

/// Extracts Jenkinsfile DSL blocks that tree-sitter-groovy parses as
/// `method_invocation` nodes: `pipeline { }`, `stage('Name') { }`,
/// `node('label') { }`, and `parallel(...)`.
fn extract_dsl_block(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    let method_name = children(node)
        .find(|n| n.kind() == "identifier")
        .map(|n| node_text(n, source));
    let method_name = match method_name {
        Some(n) => n,
        None => return,
    };

    match method_name.as_str() {
        "pipeline" => {
            symbols.push(ExtractedSymbol {
                name: "pipeline".to_string(),
                kind: SymbolKind::Workflow,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: extract_signature(node, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
        }
        "stage" => {
            let name = extract_first_string_arg(node, source)
                .unwrap_or_else(|| "unnamed".to_string());
            symbols.push(ExtractedSymbol {
                name,
                kind: SymbolKind::Stage,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: extract_signature(node, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
        }
        "node" => {
            let label = extract_first_string_arg(node, source)
                .unwrap_or_else(|| "default".to_string());
            symbols.push(ExtractedSymbol {
                name: format!("node:{label}"),
                kind: SymbolKind::Job,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: extract_signature(node, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
        }
        "parallel" => {
            symbols.push(ExtractedSymbol {
                name: "parallel".to_string(),
                kind: SymbolKind::Job,
                line_start: node.start_position().row as u32 + 1,
                line_end: node.end_position().row as u32 + 1,
                signature: extract_signature(node, source),
                is_exported: true,
                parent_idx: None,
                unused_excluded: false,
                complexity: None,
            });
        }
        _ => {}
    }
}

/// Extracts `def VAR = value` local variable declarations.
fn extract_local_variable(node: Node, source: &[u8], symbols: &mut Vec<ExtractedSymbol>) {
    // Only extract top-level variable declarations (program scope).
    // Variables inside closures are typically DSL keywords like `agent any`
    // that tree-sitter-groovy misparses as variable declarations.
    let parent = match node.parent() {
        Some(p) => p,
        None => return,
    };
    if parent.kind() != "program" {
        return;
    }

    for child in children(node) {
        if child.kind() == "variable_declarator"
            && let Some(name_node) = children(child).find(|n| n.kind() == "identifier") {
                let name = node_text(name_node, source);
                if !name.is_empty() {
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Variable,
                        line_start: node.start_position().row as u32 + 1,
                        line_end: node.end_position().row as u32 + 1,
                        signature: extract_signature(node, source),
                        is_exported: false,
                        parent_idx: None,
                        unused_excluded: false,
                        complexity: None,
                    });
                }
            }
    }
}

/// Extracts bare `VAR = value` assignment expressions.
fn extract_assignment(node: Node, source: &[u8]) -> Option<ExtractedSymbol> {
    let parent = node.parent()?;
    // Only extract from expression_statement whose parent is program or closure
    let grandparent = parent.parent()?;
    if grandparent.kind() != "program" && grandparent.kind() != "closure" {
        return None;
    }

    let name = children(node)
        .find(|n| n.kind() == "identifier")
        .map(|n| node_text(n, source))?;
    if name.is_empty() {
        return None;
    }
    Some(ExtractedSymbol {
        name,
        kind: SymbolKind::Variable,
        line_start: parent.start_position().row as u32 + 1,
        line_end: parent.end_position().row as u32 + 1,
        signature: extract_signature(parent, source),
        is_exported: false,
        parent_idx: None,
        unused_excluded: false,
        complexity: None,
    })
}

/// Extracts `import foo.bar.Baz` declarations.
fn extract_import(node: Node, source: &[u8], imports: &mut Vec<ExtractedImport>) {
    let text = node_text(node, source);
    let path = text
        .strip_prefix("import")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !path.is_empty() {
        imports.push(ExtractedImport {
            source: path,
            specifiers: vec![],
            is_reexport: false,
        });
    }
}

/// Extracts the first string argument from a method call's argument list.
/// Handles both `character_literal` ('single-quoted') and `string_literal`
/// ("double-quoted") forms used by tree-sitter-groovy.
fn extract_first_string_arg(node: Node, source: &[u8]) -> Option<String> {
    let args = children(node).find(|n| n.kind() == "argument_list")?;
    for child in children(args) {
        match child.kind() {
            "character_literal" => {
                return Some(unquote(node_text(child, source)));
            }
            "string_literal" => {
                return Some(unquote(node_text(child, source)));
            }
            _ => {}
        }
    }
    None
}

/// Strips surrounding single or double quotes from a string.
fn unquote(s: String) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_signature(node: Node, source: &[u8]) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let text = std::str::from_utf8(&source[start..end]).ok()?;
    let first_line = text.lines().next().unwrap_or(text).trim();
    if first_line.is_empty() {
        return None;
    }
    let truncated = if first_line.len() > 200 {
        &first_line[..200]
    } else {
        first_line
    };
    Some(truncated.to_string())
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    std::str::from_utf8(&source[start..end])
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_jenkinsfile(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_groovy::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = JenkinsfileSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_pipeline_block() {
        let result = parse_jenkinsfile(
            r#"pipeline {
    agent any
}
"#,
        );
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "pipeline");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Workflow));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_stage_definition() {
        let result = parse_jenkinsfile(
            r#"stage('Build') {
    steps {
        sh 'make build'
    }
}
"#,
        );
        let stages: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Stage))
            .collect();
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].name, "Build");
        assert!(stages[0].is_exported);
    }

    #[test]
    fn test_function_definition() {
        let result = parse_jenkinsfile(
            r#"def deploy(String env) {
    sh "deploy.sh ${env}"
}
"#,
        );
        let funcs: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name, "deploy");
        assert!(funcs[0].is_exported);
    }

    #[test]
    fn test_class_definition() {
        let result = parse_jenkinsfile(
            r#"class Utils {
    static String greet(String name) {
        return "Hello ${name}"
    }
}
"#,
        );
        let classes: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Utils");
        assert!(classes[0].is_exported);
    }

    #[test]
    fn test_variable_assignment() {
        let result = parse_jenkinsfile(
            r#"def VERSION = "1.0"
BRANCH_NAME = "main"
"#,
        );
        let vars: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Variable))
            .collect();
        assert_eq!(vars.len(), 2);
        let names: Vec<&str> = vars.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"BRANCH_NAME"));
    }

    #[test]
    fn test_empty_file() {
        let result = parse_jenkinsfile("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
    }

    #[test]
    fn test_mixed_jenkinsfile() {
        let result = parse_jenkinsfile(
            r#"def VERSION = "2.0"

def notifySlack(String message) {
    slackSend channel: '#builds', message: message
}

pipeline {
    agent any
    stages {
        stage('Build') {
            steps {
                sh 'make build'
            }
        }
        stage('Test') {
            steps {
                sh 'make test'
            }
        }
        stage('Deploy') {
            steps {
                sh 'make deploy'
            }
        }
    }
}
"#,
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"notifySlack"));
        assert!(names.contains(&"pipeline"));
        assert!(names.contains(&"Build"));
        assert!(names.contains(&"Test"));
        assert!(names.contains(&"Deploy"));

        let workflows: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Workflow))
            .collect();
        assert_eq!(workflows.len(), 1);

        let stages: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Stage))
            .collect();
        assert_eq!(stages.len(), 3);
    }

    #[test]
    fn test_parallel_stages() {
        let result = parse_jenkinsfile(
            r#"parallel(
    linux: {
        sh 'make linux'
    },
    mac: {
        sh 'make mac'
    }
)
"#,
        );
        let jobs: Vec<&ExtractedSymbol> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Job))
            .collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "parallel");
        assert!(jobs[0].is_exported);
    }
}
