use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::Language;

use super::LanguageSupport;
use crate::index::symbols::{ExtractedSymbol, ParseResult, SymbolKind};

pub struct SystemdSupport;

static SECTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\w+)\]").unwrap());
static KEY_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\w+)\s*=\s*(.*)$").unwrap());

const IMPORTANT_KEYS: &[&str] = &[
    "Description",
    "ExecStart",
    "ExecStop",
    "ExecReload",
    "ExecStartPre",
    "ExecStartPost",
    "ExecStopPost",
    "User",
    "Group",
    "WorkingDirectory",
    "Environment",
    "EnvironmentFile",
    "Type",
    "Restart",
    "RestartSec",
    "WantedBy",
    "RequiredBy",
    "After",
    "Before",
    "Requires",
    "Wants",
    "BindsTo",
    "OnCalendar",
    "Persistent",
    "ListenStream",
    "ListenDatagram",
    "Accept",
    "What",
    "Where",
    "PathChanged",
    "PathExists",
    "PathModified",
    "Slice",
];

const EXEC_KEYS: &[&str] = &[
    "ExecStart",
    "ExecStop",
    "ExecReload",
    "ExecStartPre",
    "ExecStartPost",
    "ExecStopPost",
];

impl LanguageSupport for SystemdSupport {
    fn extensions(&self) -> &[&str] {
        &[
            "service", "timer", "socket", "mount", "target", "path", "slice", "scope",
        ]
    }

    fn language_name(&self) -> &str {
        "systemd"
    }

    fn tree_sitter_language(&self, _ext: &str) -> Language {
        Language::new(tree_sitter_yaml::LANGUAGE)
    }

    fn extract(&self, source: &[u8], _tree: &tree_sitter::Tree) -> ParseResult {
        let text = std::str::from_utf8(source).unwrap_or("");
        let mut symbols = Vec::new();
        let mut current_section_idx: Option<usize> = None;

        for (line_idx, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            let line_num = line_idx as u32 + 1;

            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if let Some(cap) = SECTION_RE.captures(line) {
                let name = cap[1].to_string();
                let idx = symbols.len();
                symbols.push(ExtractedSymbol {
                    name,
                    kind: SymbolKind::Module,
                    line_start: line_num,
                    line_end: line_num,
                    signature: None,
                    is_exported: true,
                    parent_idx: None,
                    unused_excluded: false,
                    complexity: None,
                });
                current_section_idx = Some(idx);
                continue;
            }

            if let Some(cap) = KEY_VALUE_RE.captures(line) {
                let key = cap[1].to_string();
                let value = cap[2].trim().to_string();

                if key == "Description"
                    && let Some(idx) = current_section_idx {
                        symbols[idx].signature = Some(value.clone());
                    }

                if !IMPORTANT_KEYS.contains(&key.as_str()) {
                    continue;
                }

                let kind = if EXEC_KEYS.contains(&key.as_str()) {
                    SymbolKind::Function
                } else {
                    SymbolKind::Variable
                };

                symbols.push(ExtractedSymbol {
                    name: key,
                    kind,
                    line_start: line_num,
                    line_end: line_num,
                    signature: Some(line.to_string()),
                    is_exported: true,
                    parent_idx: current_section_idx,
                    unused_excluded: false,
                    complexity: None,
                });
            }
        }

        // Extend each section's line_end to cover its directives
        let section_indices: Vec<usize> = symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.kind, SymbolKind::Module))
            .map(|(i, _)| i)
            .collect();

        for (pos, &sec_idx) in section_indices.iter().enumerate() {
            let next_section_start = section_indices
                .get(pos + 1)
                .map(|&i| symbols[i].line_start);

            let max_child_line = symbols
                .iter()
                .filter(|s| s.parent_idx == Some(sec_idx))
                .map(|s| s.line_end)
                .max();

            if let Some(end) = max_child_line {
                let capped = match next_section_start {
                    Some(next) if end >= next => next - 1,
                    _ => end,
                };
                symbols[sec_idx].line_end = capped;
            }
        }

        ParseResult {
            symbols,
            imports: Vec::new(),
            references: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_systemd(source: &str) -> ParseResult {
        let mut parser = Parser::new();
        let lang = Language::new(tree_sitter_yaml::LANGUAGE);
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let support = SystemdSupport;
        support.extract(source.as_bytes(), &tree)
    }

    #[test]
    fn test_section_header() {
        let result = parse_systemd("[Unit]\nDescription=My Service\n");
        assert_eq!(result.symbols[0].name, "Unit");
        assert!(matches!(result.symbols[0].kind, SymbolKind::Module));
        assert!(result.symbols[0].is_exported);
    }

    #[test]
    fn test_exec_start() {
        let result = parse_systemd("[Service]\nExecStart=/usr/bin/myapp --flag\n");
        let exec = result.symbols.iter().find(|s| s.name == "ExecStart").unwrap();
        assert!(matches!(exec.kind, SymbolKind::Function));
        assert_eq!(exec.signature.as_deref(), Some("ExecStart=/usr/bin/myapp --flag"));
        assert_eq!(exec.parent_idx, Some(0));
    }

    #[test]
    fn test_key_value_pairs() {
        let result = parse_systemd(
            "[Service]\nType=simple\nRestart=always\nUser=nobody\n",
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Type"));
        assert!(names.contains(&"Restart"));
        assert!(names.contains(&"User"));
        for sym in result.symbols.iter().filter(|s| s.name != "Service") {
            assert!(matches!(sym.kind, SymbolKind::Variable));
            assert!(sym.is_exported);
            assert_eq!(sym.parent_idx, Some(0));
        }
    }

    #[test]
    fn test_description() {
        let result = parse_systemd("[Unit]\nDescription=My awesome daemon\n");
        let unit = result.symbols.iter().find(|s| s.name == "Unit").unwrap();
        assert_eq!(unit.signature.as_deref(), Some("My awesome daemon"));
    }

    #[test]
    fn test_install_section() {
        let result = parse_systemd("[Install]\nWantedBy=multi-user.target\n");
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Install"));
        assert!(names.contains(&"WantedBy"));
        let wanted = result.symbols.iter().find(|s| s.name == "WantedBy").unwrap();
        assert_eq!(
            wanted.signature.as_deref(),
            Some("WantedBy=multi-user.target")
        );
    }

    #[test]
    fn test_empty_file() {
        let result = parse_systemd("");
        assert!(result.symbols.is_empty());
        assert!(result.imports.is_empty());
        assert!(result.references.is_empty());
    }

    #[test]
    fn test_comments_skipped() {
        let result = parse_systemd(
            "# This is a comment\n; Another comment\n[Unit]\nDescription=Test\n",
        );
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.iter().any(|n| n.contains('#') || n.contains(';')));
        assert!(names.contains(&"Unit"));
        assert!(names.contains(&"Description"));
    }

    #[test]
    fn test_mixed_service() {
        let result = parse_systemd(
            r#"[Unit]
Description=My Application Server
After=network.target
Requires=postgresql.service

[Service]
Type=notify
User=appuser
Group=appgroup
WorkingDirectory=/opt/myapp
Environment=NODE_ENV=production
ExecStart=/usr/bin/myapp --config /etc/myapp.conf
ExecStop=/bin/kill -SIGTERM $MAINPID
ExecReload=/bin/kill -SIGHUP $MAINPID
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
"#,
        );

        let section_names: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(section_names, &["Unit", "Service", "Install"]);

        let unit = result.symbols.iter().find(|s| s.name == "Unit").unwrap();
        assert_eq!(unit.signature.as_deref(), Some("My Application Server"));

        let exec_fns: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .map(|s| s.name.as_str())
            .collect();
        assert!(exec_fns.contains(&"ExecStart"));
        assert!(exec_fns.contains(&"ExecStop"));
        assert!(exec_fns.contains(&"ExecReload"));

        let service_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Service")
            .unwrap();
        let exec_start = result.symbols.iter().find(|s| s.name == "ExecStart").unwrap();
        assert_eq!(exec_start.parent_idx, Some(service_idx));

        let wanted = result.symbols.iter().find(|s| s.name == "WantedBy").unwrap();
        let install_idx = result
            .symbols
            .iter()
            .position(|s| s.name == "Install")
            .unwrap();
        assert_eq!(wanted.parent_idx, Some(install_idx));
    }

    #[test]
    fn test_timer_unit() {
        let result = parse_systemd(
            r#"[Unit]
Description=Run backup every hour

[Timer]
OnCalendar=hourly
Persistent=true

[Install]
WantedBy=timers.target
"#,
        );

        let section_names: Vec<&str> = result
            .symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Module))
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(section_names, &["Unit", "Timer", "Install"]);

        let on_cal = result.symbols.iter().find(|s| s.name == "OnCalendar").unwrap();
        assert!(matches!(on_cal.kind, SymbolKind::Variable));
        assert_eq!(on_cal.signature.as_deref(), Some("OnCalendar=hourly"));

        let persistent = result.symbols.iter().find(|s| s.name == "Persistent").unwrap();
        assert!(matches!(persistent.kind, SymbolKind::Variable));
    }
}
