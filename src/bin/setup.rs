// Rust guideline compliant 2026-04-13
//! `qartez-setup` — interactive IDE auto-setup wizard.
//!
//! Detects installed IDEs, presents an interactive checkbox prompt, and
//! configures MCP server entries for all selected IDEs. Replaces the seven
//! per-editor shell scripts with a single self-contained Rust binary.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::Local;
use clap::Parser;
use console::style;
use dialoguer::MultiSelect;

// -- Embedded hook assets (source of truth lives in scripts/) ----------------

const GUARD_HOOK_SH: &str = include_str!("../../scripts/qartez-guard.sh");
const SESSION_START_SH: &str = include_str!("../../scripts/qartez-session-start.sh");
const CLAUDE_MD_SNIPPET: &str = include_str!("../../scripts/CLAUDE.md.snippet");

// -- CLI ---------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "qartez-setup",
    about = "Interactive IDE setup wizard for qartez-mcp"
)]
struct Cli {
    /// Skip interactive prompt, configure all detected IDEs.
    #[arg(long)]
    yes: bool,

    /// Remove qartez configuration from all configured IDEs.
    #[arg(long)]
    uninstall: bool,

    /// Only configure specific IDEs (comma-separated or repeated).
    #[arg(long, value_delimiter = ',')]
    ide: Vec<String>,
}

// -- IDE registry ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Ide {
    ClaudeCode,
    Cursor,
    Windsurf,
    Zed,
    Continue,
    OpenCode,
    Codex,
}

impl Ide {
    const ALL: &'static [Ide] = &[
        Ide::ClaudeCode,
        Ide::Cursor,
        Ide::Windsurf,
        Ide::Zed,
        Ide::Continue,
        Ide::OpenCode,
        Ide::Codex,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Zed => "zed",
            Self::Continue => "continue",
            Self::OpenCode => "opencode",
            Self::Codex => "codex",
        }
    }

    fn from_slug(s: &str) -> Option<Self> {
        match s {
            "claude" | "claude-code" | "claudecode" => Some(Self::ClaudeCode),
            "cursor" => Some(Self::Cursor),
            "windsurf" => Some(Self::Windsurf),
            "zed" => Some(Self::Zed),
            "continue" => Some(Self::Continue),
            "opencode" => Some(Self::OpenCode),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn detection_dir(self) -> PathBuf {
        let home = home_dir();
        match self {
            Self::ClaudeCode => home.join(".claude"),
            Self::Cursor => home.join(".cursor"),
            Self::Windsurf => home.join(".codeium").join("windsurf"),
            Self::Zed => home.join(".config").join("zed"),
            Self::Continue => home.join(".continue"),
            Self::OpenCode => home.join(".config").join("opencode"),
            Self::Codex => home.join(".codex"),
        }
    }

    fn config_path(self) -> PathBuf {
        let home = home_dir();
        match self {
            Self::ClaudeCode => home.join(".claude").join("settings.json"),
            Self::Cursor => home.join(".cursor").join("mcp.json"),
            Self::Windsurf => home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            Self::Zed => home.join(".config").join("zed").join("settings.json"),
            Self::Continue => home.join(".continue").join("config.yaml"),
            Self::OpenCode => home
                .join(".config")
                .join("opencode")
                .join("opencode.json"),
            Self::Codex => home.join(".codex").join("config.toml"),
        }
    }

    fn is_detected(self) -> bool {
        match self {
            Self::ClaudeCode => !discover_claude_dirs().is_empty(),
            _ => self.detection_dir().is_dir(),
        }
    }
}

impl fmt::Display for Ide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => write!(f, "Claude Code"),
            Self::Cursor => write!(f, "Cursor"),
            Self::Windsurf => write!(f, "Windsurf"),
            Self::Zed => write!(f, "Zed"),
            Self::Continue => write!(f, "Continue"),
            Self::OpenCode => write!(f, "OpenCode"),
            Self::Codex => write!(f, "Codex"),
        }
    }
}

// -- Helpers -----------------------------------------------------------------

fn home_dir() -> PathBuf {
    dirs_replacement()
}

/// Portable home directory lookup without pulling in the `dirs` crate.
fn dirs_replacement() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("$HOME is not set")
}

/// Returns every Claude Code configuration directory found under `$HOME`:
/// the primary `~/.claude` plus any `~/.claude-<variant>` siblings
/// (e.g., `~/.claude-louis`, `~/.claude-thomas`).
///
/// Results are sorted for deterministic output, and dirs whose names look
/// like backup or temp artifacts (`.bak`, `.tmp`, `.backup`) are filtered
/// out so we never touch restore points.
fn discover_claude_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    let mut dirs: Vec<PathBuf> = Vec::new();

    let primary = home.join(".claude");
    if primary.is_dir() {
        dirs.push(primary);
    }

    let Ok(entries) = fs::read_dir(&home) else {
        return dirs;
    };

    let mut variants: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if !name.starts_with(".claude-") {
                return None;
            }
            if name.contains(".bak") || name.contains(".tmp") || name.contains(".backup") {
                return None;
            }
            let path = entry.path();
            if path.is_dir() { Some(path) } else { None }
        })
        .collect();
    variants.sort();
    dirs.extend(variants);

    dirs
}

fn info(msg: &str) {
    eprintln!("  {} {msg}", style("[+]").green());
}

fn warn(msg: &str) {
    eprintln!("  {} {msg}", style("[!]").yellow());
}

fn timestamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn backup_file(path: &Path) -> anyhow::Result<()> {
    if path.is_file() {
        let ts = timestamp();
        let backup = path.with_extension(format!(
            "{}.qartez-backup-{ts}",
            path.extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        fs::copy(path, &backup)?;
        info(&format!("Backup: {}", backup.display()));
    }
    Ok(())
}

fn ensure_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Read JSON from a file, creating `{}` if the file doesn't exist.
fn read_json(path: &Path) -> anyhow::Result<serde_json::Value> {
    if path.is_file() {
        let text = fs::read_to_string(path)?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(serde_json::json!({}));
        }
        Ok(serde_json::from_str(trimmed)?)
    } else {
        Ok(serde_json::json!({}))
    }
}

fn write_json(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    ensure_parent(path)?;
    let text = serde_json::to_string_pretty(value)? + "\n";
    fs::write(path, text)?;
    Ok(())
}

/// Strip JSONC comments (`//`, `/* */`) and trailing commas before parsing.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut in_str = false;
    let mut esc = false;

    while i < n {
        let ch = bytes[i];
        if in_str {
            out.push(ch as char);
            if esc {
                esc = false;
            } else if ch == b'\\' {
                esc = true;
            } else if ch == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
            continue;
        }
        // Line comment
        if ch == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment
        if ch == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < n {
                i += 2;
            }
            continue;
        }
        out.push(ch as char);
        i += 1;
    }
    // Strip trailing commas before } or ]
    let re = regex::Regex::new(r",(\s*[}\]])").expect("valid regex");
    re.replace_all(&out, "$1").into_owned()
}

/// Find the qartez-mcp binary, checking standard locations.
fn find_binary(name: &str) -> Option<PathBuf> {
    let home = home_dir();
    let candidates = [
        home.join(".local").join("bin").join(name),
        // Also try the cargo target dir relative to this binary
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(name)))
            .unwrap_or_default(),
    ];
    for c in &candidates {
        if c.is_file() {
            return Some(c.clone());
        }
    }
    // Check PATH
    which_in_path(name)
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// -- Per-IDE install/uninstall -----------------------------------------------

fn install_ide(ide: Ide, bin: &str, guard_bin: Option<&str>) -> anyhow::Result<()> {
    match ide {
        Ide::ClaudeCode => install_claude(bin, guard_bin),
        Ide::Cursor => install_json_mcp_servers(ide, bin),
        Ide::Windsurf => install_json_mcp_servers(ide, bin),
        Ide::Zed => install_zed(bin),
        Ide::Continue => install_continue(bin),
        Ide::OpenCode => install_opencode(bin),
        Ide::Codex => install_codex(bin),
    }
}

fn uninstall_ide(ide: Ide) -> anyhow::Result<()> {
    match ide {
        Ide::ClaudeCode => uninstall_claude(),
        Ide::Cursor | Ide::Windsurf => uninstall_json_mcp_servers(ide),
        Ide::Zed => uninstall_zed(),
        Ide::Continue => uninstall_continue(),
        Ide::OpenCode => uninstall_opencode(),
        Ide::Codex => uninstall_codex(),
    }
}

// -- Claude Code (most complex) ----------------------------------------------

fn install_claude(bin: &str, guard_bin: Option<&str>) -> anyhow::Result<()> {
    let dirs = discover_claude_dirs();
    if dirs.is_empty() {
        // No Claude Code config dirs yet — bootstrap the default one so a
        // first-time user still ends up with a working setup.
        let fallback = home_dir().join(".claude");
        return install_claude_one(&fallback, bin, guard_bin);
    }

    info(&format!(
        "Claude Code: deploying to {} directory/directories",
        dirs.len()
    ));
    for dir in &dirs {
        install_claude_one(dir, bin, guard_bin)?;
    }
    Ok(())
}

fn install_claude_one(
    claude_dir: &Path,
    bin: &str,
    guard_bin: Option<&str>,
) -> anyhow::Result<()> {
    let hooks_dir = claude_dir.join("hooks");
    let settings_path = claude_dir.join("settings.json");

    info(&format!("» {}", claude_dir.display()));

    // 1. Install hook scripts
    fs::create_dir_all(&hooks_dir)?;
    let guard_sh = hooks_dir.join("qartez-guard.sh");
    fs::write(&guard_sh, GUARD_HOOK_SH)?;
    make_executable(&guard_sh)?;
    info(&format!("Hook installed: {}", guard_sh.display()));

    let session_sh = hooks_dir.join("qartez-session-start.sh");
    fs::write(&session_sh, SESSION_START_SH)?;
    make_executable(&session_sh)?;
    info(&format!("Hook installed: {}", session_sh.display()));

    // 2. Configure settings.json
    ensure_parent(&settings_path)?;
    let mut settings = read_json(&settings_path)?;

    // Ensure hooks object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }

    // Absolute hook commands so they resolve regardless of which claude
    // home the shell's `~` currently refers to.
    let guard_cmd = format!("bash {}", guard_sh.display());
    let session_cmd = format!("bash {}", session_sh.display());

    // PreToolUse: Glob|Grep guard
    ensure_hook_entry(
        &mut settings,
        "PreToolUse",
        "Glob|Grep",
        "qartez-guard",
        &guard_cmd,
        3000,
    );

    // PreToolUse: Edit|Write|MultiEdit modification guard
    if let Some(guard) = guard_bin {
        ensure_hook_entry(
            &mut settings,
            "PreToolUse",
            "Edit|Write|MultiEdit",
            "qartez-guard",
            guard,
            3000,
        );
    } else {
        warn("qartez-guard binary not found; modification guard hook skipped");
    }

    // SessionStart: auto-indexing
    ensure_hook_entry_no_matcher(
        &mut settings,
        "SessionStart",
        "qartez-session-start",
        &session_cmd,
        5000,
    );

    // MCP server entry
    settings["mcpServers"]["qartez"] = serde_json::json!({
        "command": bin,
        "args": []
    });

    backup_file(&settings_path)?;
    write_json(&settings_path, &settings)?;
    info(&format!("Settings updated: {}", settings_path.display()));

    // 3. Install CLAUDE.md snippet
    install_claude_md_snippet(&claude_dir.join("CLAUDE.md"))?;

    Ok(())
}

fn ensure_hook_entry(
    settings: &mut serde_json::Value,
    hook_type: &str,
    matcher: &str,
    search_term: &str,
    command: &str,
    timeout: u64,
) {
    let hooks_arr = settings["hooks"][hook_type]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let already = hooks_arr.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some(matcher)
            && entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|arr| {
                    arr.iter().any(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains(search_term))
                    })
                })
    });

    if already {
        // Refresh command path
        let mut updated = hooks_arr;
        for entry in &mut updated {
            if entry.get("matcher").and_then(|m| m.as_str()) == Some(matcher)
                && let Some(arr) = entry.get_mut("hooks").and_then(|h| h.as_array_mut())
            {
                for h in arr.iter_mut() {
                    if h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains(search_term))
                    {
                        h["command"] = serde_json::Value::String(command.to_string());
                    }
                }
            }
        }
        settings["hooks"][hook_type] = serde_json::Value::Array(updated);
    } else {
        let new_entry = serde_json::json!({
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": timeout
            }]
        });
        let mut arr = hooks_arr;
        arr.push(new_entry);
        settings["hooks"][hook_type] = serde_json::Value::Array(arr);
    }
}

fn ensure_hook_entry_no_matcher(
    settings: &mut serde_json::Value,
    hook_type: &str,
    search_term: &str,
    command: &str,
    timeout: u64,
) {
    let hooks_arr = settings["hooks"][hook_type]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let already = hooks_arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|arr| {
                arr.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains(search_term))
                })
            })
    });

    if !already {
        let new_entry = serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": timeout
            }]
        });
        let mut arr = hooks_arr;
        arr.push(new_entry);
        settings["hooks"][hook_type] = serde_json::Value::Array(arr);
    }
}

fn install_claude_md_snippet(target: &Path) -> anyhow::Result<()> {
    let begin = "<!-- qartez-mcp-instructions -->";
    let end = "<!-- /qartez-mcp-instructions -->";

    ensure_parent(target)?;

    if !target.is_file() {
        fs::write(target, CLAUDE_MD_SNIPPET)?;
        info(&format!("Created {} with qartez snippet", target.display()));
        return Ok(());
    }

    let content = fs::read_to_string(target)?;
    if !content.contains(begin) {
        let mut out = content;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(CLAUDE_MD_SNIPPET);
        fs::write(target, out)?;
        info(&format!("Appended qartez snippet to {}", target.display()));
        return Ok(());
    }

    // Replace existing snippet between markers
    let mut result = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line == begin {
            skipping = true;
            result.push_str(CLAUDE_MD_SNIPPET);
            if !CLAUDE_MD_SNIPPET.ends_with('\n') {
                result.push('\n');
            }
            continue;
        }
        if line == end && skipping {
            skipping = false;
            continue;
        }
        if !skipping {
            result.push_str(line);
            result.push('\n');
        }
    }

    if result.trim() == content.trim() {
        info(&format!(
            "Qartez snippet already up to date in {}",
            target.display()
        ));
    } else {
        fs::write(target, result)?;
        info(&format!("Qartez snippet updated in {}", target.display()));
    }
    Ok(())
}

fn remove_claude_md_snippet(target: &Path) -> anyhow::Result<()> {
    let begin = "<!-- qartez-mcp-instructions -->";
    let end = "<!-- /qartez-mcp-instructions -->";

    if !target.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(target)?;
    if !content.contains(begin) {
        return Ok(());
    }

    let mut result = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line == begin {
            skipping = true;
            continue;
        }
        if line == end && skipping {
            skipping = false;
            continue;
        }
        if !skipping {
            result.push_str(line);
            result.push('\n');
        }
    }
    fs::write(target, result)?;
    info(&format!(
        "Qartez snippet removed from {}",
        target.display()
    ));
    Ok(())
}

fn install_global_gitignore() -> anyhow::Result<()> {
    let excludes = resolve_global_gitignore();
    ensure_parent(&excludes)?;

    if !excludes.is_file() {
        fs::write(&excludes, "")?;
    }

    let content = fs::read_to_string(&excludes)?;
    let already = content
        .lines()
        .any(|l| l.trim() == ".qartez/" || l.trim() == ".qartez" || l.trim() == "/.qartez/");

    if already {
        info(&format!(
            "Global gitignore already contains .qartez/ ({})",
            excludes.display()
        ));
        return Ok(());
    }

    let mut out = content;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(".qartez/\n");
    fs::write(&excludes, out)?;
    info(&format!(
        "Added .qartez/ to global gitignore: {}",
        excludes.display()
    ));
    Ok(())
}

fn resolve_global_gitignore() -> PathBuf {
    // Try git config --global core.excludesfile
    if let Ok(output) = std::process::Command::new("git")
        .args(["config", "--global", "core.excludesfile"])
        .output()
    {
        let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path_str.is_empty() {
            let expanded = if let Some(rest) = path_str.strip_prefix("~/") {
                home_dir().join(rest)
            } else {
                PathBuf::from(&path_str)
            };
            return expanded;
        }
    }
    // Default: XDG_CONFIG_HOME/git/ignore or ~/.config/git/ignore
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    xdg.join("git").join("ignore")
}

fn uninstall_claude() -> anyhow::Result<()> {
    let dirs = discover_claude_dirs();
    if dirs.is_empty() {
        return uninstall_claude_one(&home_dir().join(".claude"));
    }

    for dir in &dirs {
        uninstall_claude_one(dir)?;
    }
    info("Uninstall complete");
    Ok(())
}

fn uninstall_claude_one(claude_dir: &Path) -> anyhow::Result<()> {
    let hooks_dir = claude_dir.join("hooks");
    let settings_path = claude_dir.join("settings.json");

    info(&format!("» {}", claude_dir.display()));

    // Remove hook files
    for name in ["qartez-guard.sh", "qartez-session-start.sh"] {
        let path = hooks_dir.join(name);
        if path.is_file() {
            fs::remove_file(&path)?;
            info(&format!("Hook removed: {}", path.display()));
        }
    }

    if settings_path.is_file() {
        backup_file(&settings_path)?;
        let mut settings = read_json(&settings_path)?;

        // Remove qartez hook entries from PreToolUse
        remove_hook_entries_containing(&mut settings, "PreToolUse", "qartez-guard");
        // Remove qartez session start hook
        remove_hook_entries_containing(&mut settings, "SessionStart", "qartez-session-start");

        // Clean up empty hooks object
        if let Some(hooks) = settings.get("hooks")
            && hooks.as_object().is_some_and(|o| o.is_empty())
        {
            settings.as_object_mut().map(|o| o.remove("hooks"));
        }

        // Remove MCP server
        if let Some(servers) = settings.get_mut("mcpServers")
            && let Some(obj) = servers.as_object_mut()
        {
            obj.remove("qartez");
            if obj.is_empty() {
                settings.as_object_mut().map(|o| o.remove("mcpServers"));
            }
        }

        write_json(&settings_path, &settings)?;
        info(&format!("Settings cleaned up: {}", settings_path.display()));
    }

    remove_claude_md_snippet(&claude_dir.join("CLAUDE.md"))?;
    Ok(())
}

fn remove_hook_entries_containing(
    settings: &mut serde_json::Value,
    hook_type: &str,
    search_term: &str,
) {
    if let Some(arr) = settings["hooks"][hook_type].as_array() {
        let filtered: Vec<_> = arr
            .iter()
            .filter(|entry| {
                !entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains(search_term))
                        })
                    })
            })
            .cloned()
            .collect();
        if filtered.is_empty() {
            if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                hooks.remove(hook_type);
            }
        } else {
            settings["hooks"][hook_type] = serde_json::Value::Array(filtered);
        }
    }
}

// -- Cursor / Windsurf (shared JSON mcpServers pattern) ----------------------

fn install_json_mcp_servers(ide: Ide, bin: &str) -> anyhow::Result<()> {
    let config_path = ide.config_path();
    ensure_parent(&config_path)?;

    let mut data = read_json(&config_path)?;
    if data.get("mcpServers").is_none() {
        data["mcpServers"] = serde_json::json!({});
    }

    let current = data["mcpServers"]["qartez"]["command"]
        .as_str()
        .map(str::to_string);
    if current.as_deref() == Some(bin) {
        info(&format!(
            "{ide} already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    data["mcpServers"]["qartez"] = serde_json::json!({
        "command": bin,
        "args": []
    });
    write_json(&config_path, &data)?;

    if current.is_none() {
        info(&format!("Added qartez to {ide} config: {}", config_path.display()));
    } else {
        info(&format!("Updated qartez in {ide} config: {} -> {bin}", current.unwrap_or_default()));
    }
    Ok(())
}

fn uninstall_json_mcp_servers(ide: Ide) -> anyhow::Result<()> {
    let config_path = ide.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No {ide} config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let mut data = read_json(&config_path)?;
    let present = data
        .get("mcpServers")
        .and_then(|s| s.get("qartez"))
        .is_some();

    if !present {
        info(&format!(
            "qartez not present in {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    if let Some(servers) = data.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
        servers.remove("qartez");
    }
    write_json(&config_path, &data)?;
    info(&format!("Removed qartez from {}", config_path.display()));
    Ok(())
}

// -- Zed (JSONC with context_servers) ----------------------------------------

fn install_zed(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::Zed.config_path();
    ensure_parent(&config_path)?;

    let raw = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        "{}".to_string()
    };

    let cleaned = strip_jsonc(&raw);
    let json_str = if cleaned.trim().is_empty() { "{}" } else { &cleaned };
    let mut data: serde_json::Value = serde_json::from_str(json_str)?;

    if data.get("context_servers").is_none() {
        data["context_servers"] = serde_json::json!({});
    }

    let desired = serde_json::json!({
        "command": bin,
        "args": [],
        "env": {}
    });

    if data["context_servers"]["qartez"] == desired {
        info(&format!(
            "Zed already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    let existed = data["context_servers"]
        .get("qartez")
        .is_some();
    data["context_servers"]["qartez"] = desired;

    // Write back as clean JSON (comments stripped, but that's acceptable)
    write_json(&config_path, &data)?;
    if existed {
        info(&format!("Updated qartez in Zed config: {}", config_path.display()));
    } else {
        info(&format!("Added qartez to Zed config: {}", config_path.display()));
    }
    Ok(())
}

fn uninstall_zed() -> anyhow::Result<()> {
    let config_path = Ide::Zed.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Zed config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let raw = fs::read_to_string(&config_path)?;
    let cleaned = strip_jsonc(&raw);
    let mut data: serde_json::Value = serde_json::from_str(&cleaned)?;

    let present = data
        .get("context_servers")
        .and_then(|s| s.get("qartez"))
        .is_some();
    if !present {
        info(&format!(
            "qartez not present in {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    if let Some(servers) = data
        .get_mut("context_servers")
        .and_then(|s| s.as_object_mut())
    {
        servers.remove("qartez");
    }
    write_json(&config_path, &data)?;
    info(&format!("Removed qartez from {}", config_path.display()));
    Ok(())
}

// -- Continue (YAML with mcpServers list) ------------------------------------

fn install_continue(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::Continue.config_path();
    ensure_parent(&config_path)?;

    let mut data: serde_yaml::Value = if config_path.is_file() {
        let text = fs::read_to_string(&config_path)?;
        serde_yaml::from_str(&text).unwrap_or(serde_yaml::Value::Mapping(Default::default()))
    } else {
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("Local Config".into()),
        );
        m.insert(
            serde_yaml::Value::String("version".into()),
            serde_yaml::Value::String("0.0.1".into()),
        );
        m.insert(
            serde_yaml::Value::String("schema".into()),
            serde_yaml::Value::String("v1".into()),
        );
        serde_yaml::Value::Mapping(m)
    };

    let mapping = data
        .as_mapping_mut()
        .expect("YAML config must be a mapping");

    // Ensure mcpServers is a sequence
    let servers_key = serde_yaml::Value::String("mcpServers".into());
    if !mapping.contains_key(&servers_key)
        || !mapping[&servers_key].is_sequence()
    {
        mapping.insert(servers_key.clone(), serde_yaml::Value::Sequence(vec![]));
    }

    let servers = mapping[&servers_key]
        .as_sequence_mut()
        .expect("mcpServers must be a sequence");

    let desired = {
        let mut m = serde_yaml::Mapping::new();
        m.insert("name".into(), serde_yaml::Value::String("qartez".into()));
        m.insert("command".into(), serde_yaml::Value::String(bin.into()));
        m.insert("args".into(), serde_yaml::Value::Sequence(vec![]));
        serde_yaml::Value::Mapping(m)
    };

    // Find existing entry by name
    let idx = servers.iter().position(|entry| {
        entry
            .as_mapping()
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            == Some("qartez")
    });

    if let Some(i) = idx {
        if servers[i] == desired {
            info(&format!(
                "Continue already has qartez pointing at {bin} (no changes)."
            ));
            return Ok(());
        }
        backup_file(&config_path)?;
        servers[i] = desired;
        info(&format!(
            "Updated qartez in Continue config: {}",
            config_path.display()
        ));
    } else {
        backup_file(&config_path)?;
        servers.push(desired);
        info(&format!(
            "Added qartez to Continue config: {}",
            config_path.display()
        ));
    }

    let out = serde_yaml::to_string(&data)?;
    fs::write(&config_path, out)?;
    Ok(())
}

fn uninstall_continue() -> anyhow::Result<()> {
    let config_path = Ide::Continue.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Continue config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let text = fs::read_to_string(&config_path)?;
    let mut data: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let mapping = data
        .as_mapping_mut()
        .expect("YAML config must be a mapping");

    let servers_key = serde_yaml::Value::String("mcpServers".into());
    let Some(servers) = mapping
        .get_mut(&servers_key)
        .and_then(|v| v.as_sequence_mut())
    else {
        info("qartez not present in Continue config. Nothing to uninstall.");
        return Ok(());
    };

    let idx = servers.iter().position(|entry| {
        entry
            .as_mapping()
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            == Some("qartez")
    });

    let Some(i) = idx else {
        info("qartez not present in Continue config. Nothing to uninstall.");
        return Ok(());
    };

    backup_file(&config_path)?;
    servers.remove(i);
    let out = serde_yaml::to_string(&data)?;
    fs::write(&config_path, out)?;
    info(&format!(
        "Removed qartez from {}",
        config_path.display()
    ));
    Ok(())
}

// -- OpenCode (JSON with mcp key, command is array) --------------------------

fn install_opencode(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::OpenCode.config_path();
    ensure_parent(&config_path)?;

    let mut data = read_json(&config_path)?;
    if data.get("$schema").is_none() {
        data["$schema"] = serde_json::json!("https://opencode.ai/config.json");
    }
    if data.get("mcp").is_none() {
        data["mcp"] = serde_json::json!({});
    }

    let desired = serde_json::json!({
        "type": "local",
        "command": [bin],
        "enabled": true
    });

    if data["mcp"]["qartez"] == desired {
        info(&format!(
            "OpenCode already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    let existed = data["mcp"].get("qartez").is_some();
    backup_file(&config_path)?;
    data["mcp"]["qartez"] = desired;
    write_json(&config_path, &data)?;

    if existed {
        info(&format!(
            "Updated qartez in OpenCode config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Added qartez to OpenCode config: {}",
            config_path.display()
        ));
    }
    Ok(())
}

fn uninstall_opencode() -> anyhow::Result<()> {
    let config_path = Ide::OpenCode.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No OpenCode config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let mut data = read_json(&config_path)?;
    let present = data.get("mcp").and_then(|m| m.get("qartez")).is_some();
    if !present {
        info("qartez not present in OpenCode config. Nothing to uninstall.");
        return Ok(());
    }

    backup_file(&config_path)?;
    if let Some(mcp) = data.get_mut("mcp").and_then(|m| m.as_object_mut()) {
        mcp.remove("qartez");
    }
    write_json(&config_path, &data)?;
    info(&format!(
        "Removed qartez from {}",
        config_path.display()
    ));
    Ok(())
}

// -- Codex (TOML with begin/end markers) -------------------------------------

const CODEX_BEGIN: &str = "# <qartez-mcp-begin>";
const CODEX_END: &str = "# <qartez-mcp-end>";

fn render_codex_block(bin: &str) -> String {
    let escaped = bin.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{CODEX_BEGIN}\n[mcp_servers.qartez]\ncommand = \"{escaped}\"\nargs = []\n{CODEX_END}\n"
    )
}

fn install_codex(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::Codex.config_path();
    ensure_parent(&config_path)?;

    let content = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };

    // Check for unmanaged entry
    if has_unmanaged_codex_entry(&content) {
        anyhow::bail!(
            "{} already contains an unmanaged [mcp_servers.qartez] section. \
             Remove it manually and re-run.",
            config_path.display()
        );
    }

    let current = current_codex_binary(&content);
    if current.as_deref() == Some(bin) {
        info(&format!(
            "Codex already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    backup_file(&config_path)?;

    let mut out = remove_codex_block(&content);
    // Ensure trailing newline + blank line before appending
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&render_codex_block(bin));

    fs::write(&config_path, out)?;

    if current.is_some() {
        info(&format!(
            "Updated qartez in Codex config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Added qartez to Codex config: {}",
            config_path.display()
        ));
    }
    Ok(())
}

fn uninstall_codex() -> anyhow::Result<()> {
    let config_path = Ide::Codex.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Codex config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let content = fs::read_to_string(&config_path)?;
    if !content.contains(CODEX_BEGIN) {
        info("qartez not present in Codex config. Nothing to uninstall.");
        return Ok(());
    }

    backup_file(&config_path)?;
    let out = remove_codex_block(&content);
    fs::write(&config_path, out)?;
    info(&format!(
        "Removed qartez from {}",
        config_path.display()
    ));
    Ok(())
}

fn remove_codex_block(content: &str) -> String {
    let mut result = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line == CODEX_BEGIN {
            skipping = true;
            continue;
        }
        if line == CODEX_END && skipping {
            skipping = false;
            continue;
        }
        if !skipping {
            result.push_str(line);
            result.push('\n');
        }
    }
    // Trim excessive trailing blank lines to max one
    while result.ends_with("\n\n") {
        result.pop();
    }
    result
}

fn has_unmanaged_codex_entry(content: &str) -> bool {
    let mut in_block = false;
    for line in content.lines() {
        if line == CODEX_BEGIN {
            in_block = true;
            continue;
        }
        if line == CODEX_END {
            in_block = false;
            continue;
        }
        if !in_block && line.trim() == "[mcp_servers.qartez]" {
            return true;
        }
    }
    false
}

fn current_codex_binary(content: &str) -> Option<String> {
    let mut in_block = false;
    let mut in_section = false;
    for line in content.lines() {
        if line == CODEX_BEGIN {
            in_block = true;
            continue;
        }
        if line == CODEX_END {
            in_block = false;
            in_section = false;
            continue;
        }
        if in_block && line.trim() == "[mcp_servers.qartez]" {
            in_section = true;
            continue;
        }
        if in_block && in_section && line.trim_start().starts_with("command") {
            // Parse: command = "path"
            if let Some(val) = line.split('=').nth(1) {
                let trimmed = val.trim().trim_matches('"');
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

// -- Platform helpers --------------------------------------------------------

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

// -- Main --------------------------------------------------------------------

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("  {} {e:#}", style("[x]").red());
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve binaries
    let bin = find_binary("qartez-mcp").map(|p| p.to_string_lossy().into_owned());
    let guard_bin = find_binary("qartez-guard").map(|p| p.to_string_lossy().into_owned());

    if bin.is_none() && !cli.uninstall {
        anyhow::bail!(
            "qartez-mcp binary not found. Searched:\n\
             \x20   - ~/.local/bin/qartez-mcp\n\
             \x20   - $PATH\n\n\
             \x20   Run 'make build && make deploy' first."
        );
    }
    let bin_path = bin.unwrap_or_default();

    // Uninstall mode
    if cli.uninstall {
        return run_uninstall(&cli);
    }

    // Determine which IDEs to configure
    let selected = select_ides(&cli)?;
    if selected.is_empty() {
        eprintln!("  No IDEs selected. Nothing to do.");
        return Ok(());
    }

    // Install for each selected IDE
    let mut any_error = false;
    for ide in &selected {
        eprint!("  {} Configuring {ide}...", style("[+]").green());
        match install_ide(*ide, &bin_path, guard_bin.as_deref()) {
            Ok(()) => {
                eprintln!(" done");
            }
            Err(e) => {
                eprintln!(" {}", style(format!("error: {e:#}")).red());
                any_error = true;
            }
        }
    }

    // Global gitignore applies regardless of which IDE was selected —
    // .qartez/ directories are created for any IDE, not just Claude Code.
    if let Err(e) = install_global_gitignore() {
        eprintln!(
            "  {} Failed to update global gitignore: {e:#}",
            style("[!]").yellow()
        );
    }

    eprintln!();
    if any_error {
        eprintln!(
            "  {} Setup completed with errors. Check messages above.",
            style("[!]").yellow()
        );
    } else {
        eprintln!(
            "  {} Setup complete! Restart your IDEs for changes to take effect.",
            style("✓").green().bold()
        );
    }
    Ok(())
}

fn run_uninstall(cli: &Cli) -> anyhow::Result<()> {
    let ides: Vec<Ide> = if !cli.ide.is_empty() {
        parse_ide_list(&cli.ide)?
    } else {
        Ide::ALL.to_vec()
    };

    for ide in &ides {
        eprint!("  {} Removing qartez from {ide}...", style("[-]").red());
        match uninstall_ide(*ide) {
            Ok(()) => eprintln!(" done"),
            Err(e) => eprintln!(" {}", style(format!("error: {e:#}")).red()),
        }
    }

    eprintln!();
    eprintln!(
        "  {} Uninstall complete.",
        style("✓").green().bold()
    );
    Ok(())
}

fn select_ides(cli: &Cli) -> anyhow::Result<Vec<Ide>> {
    // If --ide is specified, use those
    if !cli.ide.is_empty() {
        return parse_ide_list(&cli.ide);
    }

    let detected: Vec<(Ide, bool)> = Ide::ALL
        .iter()
        .map(|ide| (*ide, ide.is_detected()))
        .collect();

    // If --yes, configure all detected
    if cli.yes {
        return Ok(detected
            .iter()
            .filter(|(_, det)| *det)
            .map(|(ide, _)| *ide)
            .collect());
    }

    // Interactive mode
    eprintln!();
    let items: Vec<String> = detected
        .iter()
        .map(|(ide, det)| {
            if *det {
                let detail = match ide {
                    Ide::ClaudeCode => {
                        let dirs = discover_claude_dirs();
                        match dirs.len() {
                            0 | 1 => ide.detection_dir().display().to_string(),
                            n => format!(
                                "{n} dirs: {}",
                                dirs.iter()
                                    .map(|d| d.display().to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        }
                    }
                    _ => ide.detection_dir().display().to_string(),
                };
                format!("{:<16} ({})", ide.to_string(), style(detail).dim())
            } else {
                format!(
                    "{:<16} ({})",
                    ide.to_string(),
                    style("not installed").dim()
                )
            }
        })
        .collect();

    let defaults: Vec<bool> = detected.iter().map(|(_, det)| *det).collect();

    let selections = MultiSelect::new()
        .with_prompt("  Select IDEs to configure (Space to toggle, Enter to confirm)")
        .items(&items)
        .defaults(&defaults)
        .interact()?;

    Ok(selections
        .into_iter()
        .map(|i| detected[i].0)
        .collect())
}

fn parse_ide_list(names: &[String]) -> anyhow::Result<Vec<Ide>> {
    let mut result = Vec::new();
    let mut seen = HashMap::new();
    for name in names {
        let lower = name.to_lowercase();
        if let Some(ide) = Ide::from_slug(&lower) {
            if seen.insert(ide, ()).is_none() {
                result.push(ide);
            }
        } else {
            let known: Vec<&str> = Ide::ALL.iter().map(|i| i.slug()).collect();
            anyhow::bail!(
                "Unknown IDE '{name}'. Known: {}",
                known.join(", ")
            );
        }
    }
    Ok(result)
}
