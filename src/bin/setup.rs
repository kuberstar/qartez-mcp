// Rust guideline compliant 2026-04-15
//! `qartez-setup` - interactive IDE auto-setup wizard.
//!
//! Detects installed IDEs, presents an interactive checkbox prompt, and
//! configures MCP server entries for all selected IDEs. Replaces the seven
//! per-editor shell scripts with a single self-contained Rust binary.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime};

use chrono::Local;
use clap::Parser;
use console::style;
use dialoguer::MultiSelect;

// -- Embedded hook assets (source of truth lives in scripts/) ----------------

const CLAUDE_MD_SNIPPET: &str = include_str!("../../scripts/CLAUDE.md.snippet");
const AGENTS_MD_SNIPPET: &str = include_str!("../../scripts/AGENTS.md.snippet");
const CURSOR_RULE_MDC: &str = include_str!("../../scripts/cursor-rule.mdc");
const GEMINI_MD_SNIPPET: &str = include_str!("../../scripts/GEMINI.md.snippet");

/// Full instructions template for IDEs without a skill mechanism.
/// Claude Code gets a minimal snippet instead (the skill covers everything).
const INSTRUCTIONS_MD: &str = include_str!("../../scripts/instructions.md");

// Skill files installed into ~/.claude/skills/qartez/ during setup.
const SKILL_MD: &str = include_str!("../../scripts/skill/SKILL.md");
const SKILL_TOOLS_MD: &str = include_str!("../../scripts/skill/references/tools.md");
const SKILL_GUARD_MD: &str = include_str!("../../scripts/skill/references/guard.md");

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

    /// Check for a newer release on GitHub and rebuild + reinstall if available.
    #[arg(long)]
    update: bool,

    /// Internal flag: like --update, but throttled by ~/.qartez/last-update-check
    /// (24h TTL) and silent on no-op. Used by qartez-mcp on startup.
    #[arg(long, hide = true)]
    update_background: bool,

    /// Internal flag: session-start hook entry point.
    /// Implements the auto-indexing behavior from qartez-session-start.sh in Rust.
    /// Detects project, checks for .qartez marker, validates repo markers,
    /// locates qartez-mcp binary, and spawns a detached background reindex.
    #[arg(long, hide = true)]
    session_start: bool,
}

// -- IDE registry ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Ide {
    ClaudeCode,
    Gemini,
    Cursor,
    Windsurf,
    Zed,
    Continue,
    OpenCode,
    Codex,
    Kiro,
    ClaudeDesktop,
    CopilotCli,
    AmazonQ,
    Amp,
    Goose,
    Cline,
    RooCode,
    Warp,
    Augment,
    Antigravity,
}

impl Ide {
    const ALL: &'static [Ide] = &[
        Ide::ClaudeCode,
        Ide::ClaudeDesktop,
        Ide::Gemini,
        Ide::Cursor,
        Ide::Windsurf,
        Ide::Kiro,
        Ide::Zed,
        Ide::Continue,
        Ide::CopilotCli,
        Ide::AmazonQ,
        Ide::Amp,
        Ide::Cline,
        Ide::RooCode,
        Ide::Goose,
        Ide::Warp,
        Ide::Augment,
        Ide::OpenCode,
        Ide::Codex,
        Ide::Antigravity,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::Zed => "zed",
            Self::Continue => "continue",
            Self::OpenCode => "opencode",
            Self::Codex => "codex",
            Self::Kiro => "kiro",
            Self::ClaudeDesktop => "claude-desktop",
            Self::CopilotCli => "copilot",
            Self::AmazonQ => "amazonq",
            Self::Amp => "amp",
            Self::Goose => "goose",
            Self::Cline => "cline",
            Self::RooCode => "roo",
            Self::Warp => "warp",
            Self::Augment => "augment",
            Self::Antigravity => "antigravity",
        }
    }

    fn from_slug(s: &str) -> Option<Self> {
        match s {
            "claude" | "claude-code" | "claudecode" => Some(Self::ClaudeCode),
            "claude-desktop" | "claudedesktop" => Some(Self::ClaudeDesktop),
            "gemini" | "gemini-cli" | "geminicli" => Some(Self::Gemini),
            "cursor" => Some(Self::Cursor),
            "windsurf" => Some(Self::Windsurf),
            "zed" => Some(Self::Zed),
            "continue" => Some(Self::Continue),
            "opencode" => Some(Self::OpenCode),
            "codex" => Some(Self::Codex),
            "kiro" => Some(Self::Kiro),
            "copilot" | "copilot-cli" | "copilotcli" => Some(Self::CopilotCli),
            "amazonq" | "amazon-q" | "q" => Some(Self::AmazonQ),
            "amp" => Some(Self::Amp),
            "goose" => Some(Self::Goose),
            "cline" => Some(Self::Cline),
            "roo" | "roo-code" | "roocode" => Some(Self::RooCode),
            "warp" => Some(Self::Warp),
            "augment" => Some(Self::Augment),
            "antigravity" | "google-antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }

    fn detection_dir(self) -> PathBuf {
        let home = home_dir();
        match self {
            Self::ClaudeCode => home.join(".claude"),
            Self::Gemini => home.join(".gemini"),
            Self::Cursor => home.join(".cursor"),
            Self::Windsurf => home.join(".codeium").join("windsurf"),
            Self::Zed => home.join(".config").join("zed"),
            Self::Continue => home.join(".continue"),
            Self::OpenCode => home.join(".config").join("opencode"),
            Self::Codex => home.join(".codex"),
            Self::Kiro => home.join(".kiro"),
            Self::ClaudeDesktop => claude_desktop_dir(),
            Self::CopilotCli => home.join(".copilot"),
            Self::AmazonQ => home.join(".aws").join("amazonq"),
            Self::Amp => home.join(".config").join("amp"),
            Self::Goose => home.join(".config").join("goose"),
            Self::Cline => vscode_global_storage().join("saoudrizwan.claude-dev"),
            Self::RooCode => vscode_global_storage().join("rooveterinaryinc.roo-cline"),
            Self::Warp => home.join(".warp"),
            Self::Augment => home.join(".augment"),
            Self::Antigravity => home.join(".gemini").join("antigravity"),
        }
    }

    fn config_path(self) -> PathBuf {
        let home = home_dir();
        match self {
            Self::ClaudeCode => home.join(".claude").join("settings.json"),
            Self::Gemini => home.join(".gemini").join("settings.json"),
            Self::Cursor => home.join(".cursor").join("mcp.json"),
            Self::Windsurf => home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            Self::Zed => home.join(".config").join("zed").join("settings.json"),
            Self::Continue => home.join(".continue").join("config.yaml"),
            Self::OpenCode => home.join(".config").join("opencode").join("opencode.json"),
            Self::Codex => home.join(".codex").join("config.toml"),
            Self::Kiro => home.join(".kiro").join("settings").join("mcp.json"),
            Self::ClaudeDesktop => claude_desktop_dir().join("claude_desktop_config.json"),
            Self::CopilotCli => home.join(".copilot").join("mcp-config.json"),
            Self::AmazonQ => home.join(".aws").join("amazonq").join("mcp.json"),
            Self::Amp => home.join(".config").join("amp").join("settings.json"),
            Self::Goose => home.join(".config").join("goose").join("config.yaml"),
            Self::Cline => vscode_global_storage()
                .join("saoudrizwan.claude-dev")
                .join("settings")
                .join("cline_mcp_settings.json"),
            Self::RooCode => vscode_global_storage()
                .join("rooveterinaryinc.roo-cline")
                .join("settings")
                .join("cline_mcp_settings.json"),
            Self::Warp => home.join(".warp").join("mcp_settings.json"),
            Self::Augment => home.join(".augment").join("settings.json"),
            Self::Antigravity => home
                .join(".gemini")
                .join("antigravity")
                .join("mcp_config.json"),
        }
    }

    /// CLI binary names to look for on `$PATH`. Empty means the IDE is a
    /// GUI-only app detected via its `.app` bundle or config directory alone.
    fn cli_binary_names(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &["claude"],
            Self::Gemini => &["gemini"],
            Self::Cursor => &["cursor"],
            Self::Windsurf => &["windsurf"],
            Self::Zed => &["zed", "zed-editor"],
            Self::Continue => &[],
            Self::OpenCode => &["opencode"],
            Self::Codex => &["codex"],
            Self::Kiro => &["kiro"],
            Self::ClaudeDesktop => &[],
            Self::CopilotCli => &["github-copilot"],
            Self::AmazonQ => &["q"],
            Self::Amp => &["amp"],
            Self::Goose => &["goose"],
            Self::Cline => &[],
            Self::RooCode => &[],
            Self::Warp => &["warp"],
            Self::Augment => &[],
            Self::Antigravity => &[],
        }
    }

    /// macOS `.app` bundle names (checked in `/Applications`).
    #[cfg(target_os = "macos")]
    fn app_bundle_names(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeDesktop => &["Claude.app"],
            Self::Cursor => &["Cursor.app"],
            Self::Windsurf => &["Windsurf.app"],
            Self::Zed => &["Zed.app"],
            Self::Kiro => &["Kiro.app"],
            Self::Warp => &["Warp.app"],
            Self::Antigravity => &["Antigravity.app"],
            _ => &[],
        }
    }

    fn is_detected(self) -> bool {
        let has_config = match self {
            Self::ClaudeCode => !discover_claude_dirs().is_empty(),
            Self::Gemini => !discover_gemini_dirs().is_empty(),
            _ => self.detection_dir().is_dir(),
        };
        if !has_config {
            return false;
        }
        // Config dir exists; verify the IDE is actually installed by checking
        // for a CLI binary on PATH or a macOS .app bundle.
        let bins = self.cli_binary_names();
        if bins.iter().any(|name| which_in_path(name).is_some()) {
            return true;
        }
        #[cfg(target_os = "macos")]
        {
            let apps = self.app_bundle_names();
            let app_dir = PathBuf::from("/Applications");
            if apps.iter().any(|name| app_dir.join(name).is_dir()) {
                return true;
            }
        }
        // VS Code extensions (Cline, RooCode, Continue, Augment): if the
        // config dir exists the extension is installed, no separate binary.
        matches!(
            self,
            Self::Cline | Self::RooCode | Self::Continue | Self::Augment
        )
    }
}

impl fmt::Display for Ide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => write!(f, "Claude Code"),
            Self::ClaudeDesktop => write!(f, "Claude Desktop"),
            Self::Gemini => write!(f, "Gemini CLI"),
            Self::Cursor => write!(f, "Cursor"),
            Self::Windsurf => write!(f, "Windsurf"),
            Self::Kiro => write!(f, "Kiro"),
            Self::Zed => write!(f, "Zed"),
            Self::Continue => write!(f, "Continue"),
            Self::CopilotCli => write!(f, "Copilot CLI"),
            Self::AmazonQ => write!(f, "Amazon Q"),
            Self::Amp => write!(f, "Amp"),
            Self::Cline => write!(f, "Cline"),
            Self::RooCode => write!(f, "Roo Code"),
            Self::Goose => write!(f, "Goose"),
            Self::Warp => write!(f, "Warp"),
            Self::Augment => write!(f, "Augment"),
            Self::OpenCode => write!(f, "OpenCode"),
            Self::Codex => write!(f, "Codex"),
            Self::Antigravity => write!(f, "Antigravity"),
        }
    }
}

// -- Helpers -----------------------------------------------------------------

fn home_dir() -> PathBuf {
    dirs_replacement()
}

/// Portable home directory lookup without pulling in the `dirs` crate.
/// Checks HOME (Unix), USERPROFILE (Windows), HOMEDRIVE+HOMEPATH (Windows),
/// then falls back to current directory.
fn dirs_replacement() -> PathBuf {
    // Try HOME (Unix)
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home);
    }
    // Try USERPROFILE (Windows)
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile);
    }
    // Try HOMEDRIVE+HOMEPATH (Windows fallback)
    if let (Some(drive), Some(path)) = (
        std::env::var_os("HOMEDRIVE"),
        std::env::var_os("HOMEPATH"),
    ) {
        let mut combined = PathBuf::from(drive);
        combined.push(path);
        if combined.is_dir() {
            return combined;
        }
    }
    // Fallback to current directory
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Claude Desktop's config directory varies by platform.
fn claude_desktop_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Claude")
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("Claude")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("Claude")
    }
}

/// VS Code stores extension data in a platform-specific global storage path.
fn vscode_global_storage() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("globalStorage")
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("Code")
            .join("User")
            .join("globalStorage")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("Code")
            .join("User")
            .join("globalStorage")
    }
}

fn discover_claude_dirs() -> Vec<PathBuf> {
    discover_prefixed_dirs(".claude")
}

fn discover_gemini_dirs() -> Vec<PathBuf> {
    discover_prefixed_dirs(".gemini")
}

/// Generic directory discovery for tools that support multiple configuration
/// directories via dotfile prefixes (e.g., `~/.claude`, `~/.claude-foo`).
fn discover_prefixed_dirs(prefix: &str) -> Vec<PathBuf> {
    let home = home_dir();
    let mut dirs: Vec<PathBuf> = Vec::new();

    let primary = home.join(prefix);
    if primary.is_dir() {
        dirs.push(primary);
    }

    let Ok(entries) = fs::read_dir(&home) else {
        return dirs;
    };

    let variant_prefix = format!("{}-", prefix);
    let mut variants: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if !name.starts_with(&variant_prefix) {
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

/// Returns Claude Code's runtime state file (`.claude.json`) for a given
/// Claude config directory.
///
/// Claude Code persists active MCP server entries here, and this file takes
/// precedence over `settings.json` for accounts that have one - so we must
/// touch both to make qartez visible to the CLI on next launch.
///
/// Path layout differs between the default account and named variants:
/// - Default `~/.claude/` → state at `~/.claude.json` (one level up).
/// - Variant `~/.claude-<name>/` → state at `~/.claude-<name>/.claude.json`
///   (inside the variant directory).
fn claude_state_file(claude_dir: &Path) -> PathBuf {
    let is_default = claude_dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == ".claude");
    if is_default {
        claude_dir.parent().map_or_else(
            || claude_dir.join(".claude.json"),
            |p| p.join(".claude.json"),
        )
    } else {
        claude_dir.join(".claude.json")
    }
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
        // On Windows, try with .exe suffix
        if cfg!(windows) {
            let with_exe = c.with_extension("exe");
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
    }
    // Check PATH
    which_in_path(name)
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        // Try the bare name first
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // On Windows, also try with common executable extensions
        if cfg!(windows) {
            for ext in &["exe", "cmd", "bat", "com"] {
                let with_ext = dir.join(format!("{}.{}", name, ext));
                if with_ext.is_file() {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

// -- Per-IDE install/uninstall -----------------------------------------------

fn install_ide(ide: Ide, bin: &str, guard_bin: Option<&str>) -> anyhow::Result<()> {
    match ide {
        Ide::ClaudeCode => install_claude(bin, guard_bin)?,
        Ide::Gemini => install_gemini(bin, guard_bin)?,
        Ide::Cursor => {
            install_json_mcp_servers(ide, bin)?;
            install_cursor_rule(&ide.detection_dir())?;
        }
        Ide::Windsurf => {
            install_json_mcp_servers(ide, bin)?;
            install_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::Kiro => {
            install_json_mcp_servers(ide, bin)?;
            install_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::Warp => {
            install_json_mcp_servers(ide, bin)?;
            install_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::ClaudeDesktop | Ide::AmazonQ | Ide::Antigravity => {
            install_json_mcp_servers(ide, bin)?;
        }
        Ide::Cline | Ide::RooCode => {
            install_json_mcp_servers(ide, bin)?;
            install_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::Augment => {
            install_augment(bin)?;
            install_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::CopilotCli => install_copilot_cli(bin)?,
        Ide::Amp => {
            install_amp(bin)?;
            install_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::Goose => install_goose(bin)?,
        Ide::Zed => install_zed(bin)?,
        Ide::Continue => install_continue(bin)?,
        Ide::OpenCode => {
            install_opencode(bin)?;
            let agents_path = home_dir()
                .join(".config")
                .join("opencode")
                .join("AGENTS.md");
            install_agents_md_snippet(&agents_path)?;
        }
        Ide::Codex => {
            install_codex(bin)?;
            let agents_path = home_dir().join(".codex").join("AGENTS.md");
            install_agents_md_snippet(&agents_path)?;
        }
    }
    Ok(())
}

fn uninstall_ide(ide: Ide) -> anyhow::Result<()> {
    match ide {
        Ide::ClaudeCode => uninstall_claude()?,
        Ide::Gemini => uninstall_gemini()?,
        Ide::Cursor => {
            uninstall_json_mcp_servers(ide)?;
            remove_cursor_rule(&ide.detection_dir())?;
        }
        Ide::Windsurf | Ide::Kiro | Ide::Warp => {
            uninstall_json_mcp_servers(ide)?;
            remove_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::ClaudeDesktop | Ide::AmazonQ | Ide::Antigravity => {
            uninstall_json_mcp_servers(ide)?;
        }
        Ide::Cline | Ide::RooCode => {
            uninstall_json_mcp_servers(ide)?;
            remove_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::Augment => {
            uninstall_augment()?;
            remove_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::CopilotCli => uninstall_copilot_cli()?,
        Ide::Amp => {
            uninstall_amp()?;
            remove_rules_file(&ide.detection_dir().join("rules"))?;
        }
        Ide::Goose => uninstall_goose()?,
        Ide::Zed => uninstall_zed()?,
        Ide::Continue => uninstall_continue()?,
        Ide::OpenCode => {
            uninstall_opencode()?;
            let agents_path = home_dir()
                .join(".config")
                .join("opencode")
                .join("AGENTS.md");
            remove_agents_md_snippet(&agents_path)?;
        }
        Ide::Codex => {
            uninstall_codex()?;
            let agents_path = home_dir().join(".codex").join("AGENTS.md");
            remove_agents_md_snippet(&agents_path)?;
        }
    }
    Ok(())
}

// -- Claude Code (most complex) ----------------------------------------------

fn install_claude(bin: &str, guard_bin: Option<&str>) -> anyhow::Result<()> {
    let dirs = discover_claude_dirs();
    if dirs.is_empty() {
        // No Claude Code config dirs yet - bootstrap the default one so a
        // first-time user still ends up with a working setup.
        let fallback = home_dir().join(".claude");
        install_claude_one(&fallback, bin, guard_bin)?;
        install_claude_md_snippet(&fallback.join("CLAUDE.md"))?;
        install_skill(&fallback)?;
        return Ok(());
    }

    info(&format!(
        "Claude Code: deploying to {} directory/directories",
        dirs.len()
    ));
    for dir in &dirs {
        install_claude_one(dir, bin, guard_bin)?;
    }

    // CLAUDE.md snippet goes only into ~/.claude, not into variant dirs
    let primary = home_dir().join(".claude");
    install_claude_md_snippet(&primary.join("CLAUDE.md"))?;

    // Install skill into ~/.claude/skills/qartez/ (all variants share it)
    install_skill(&primary)?;

    Ok(())
}

fn install_claude_one(claude_dir: &Path, bin: &str, _guard_bin: Option<&str>) -> anyhow::Result<()> {
    let hooks_dir = claude_dir.join("hooks");
    let settings_path = claude_dir.join("settings.json");

    info(&format!("» {}", claude_dir.display()));

    // Remove legacy shell hook wrappers. Hooks are configured to invoke
    // binaries directly, so shell scripts are no longer required.
    remove_legacy_hook_files(&hooks_dir)?;

    // 2. Configure settings.json
    ensure_parent(&settings_path)?;
    let mut settings = read_json(&settings_path)?;

    // Ensure hooks object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }

    // Use qartez-guard binary directly (no bash dependency)
    let guard_bin_path = find_binary("qartez-guard").map(|p| p.to_string_lossy().into_owned());

    // Use qartez-setup --session-start for session start hook (no bash dependency)
    let setup_bin_path = find_binary("qartez-setup")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "qartez-setup".to_string());
    let session_cmd = format!("{} --session-start", setup_bin_path);

    // PreToolUse: Glob|Grep guard
    if let Some(guard) = guard_bin_path.as_deref() {
        ensure_hook_entry(
            &mut settings,
            "PreToolUse",
            "Glob|Grep",
            "qartez-guard",
            guard,
            3000,
        );
    } else {
        warn("qartez-guard binary not found; glob/grep guard hook skipped");
    }

    // PreToolUse: Edit|Write|MultiEdit modification guard
    if let Some(guard) = guard_bin_path.as_deref() {
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

    // 3. Mirror the MCP entry into Claude Code's runtime state file
    //    (`.claude.json`). Without this, accounts that already have a state
    //    file won't pick up qartez until the user manually runs
    //    `claude mcp add`.
    let state_path = claude_state_file(claude_dir);
    if state_path.is_file() {
        backup_file(&state_path)?;
        let mut state = read_json(&state_path)?;
        if state.get("mcpServers").is_none() {
            state["mcpServers"] = serde_json::json!({});
        }
        state["mcpServers"]["qartez"] = serde_json::json!({
            "type": "stdio",
            "command": bin,
            "args": [],
            "env": {}
        });
        write_json(&state_path, &state)?;
        info(&format!("State updated: {}", state_path.display()));
    }

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

    if already {
        // Refresh command path (mirrors ensure_hook_entry behavior)
        let mut updated = hooks_arr;
        for entry in &mut updated {
            if let Some(arr) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) {
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
    install_snippet(
        target,
        CLAUDE_MD_SNIPPET,
        "<!-- qartez-mcp-instructions -->",
        "<!-- /qartez-mcp-instructions -->",
    )
}

fn remove_claude_md_snippet(target: &Path) -> anyhow::Result<()> {
    remove_snippet(
        target,
        "<!-- qartez-mcp-instructions -->",
        "<!-- /qartez-mcp-instructions -->",
    )
}

// -- Gemini CLI --------------------------------------------------------------

fn install_gemini(bin: &str, guard_bin: Option<&str>) -> anyhow::Result<()> {
    let dirs = discover_gemini_dirs();
    if dirs.is_empty() {
        let fallback = home_dir().join(".gemini");
        return install_gemini_one(&fallback, bin, guard_bin);
    }

    info(&format!(
        "Gemini CLI: deploying to {} directory/directories",
        dirs.len()
    ));
    for dir in &dirs {
        install_gemini_one(dir, bin, guard_bin)?;
    }
    Ok(())
}

fn install_gemini_one(gemini_dir: &Path, bin: &str, _guard_bin: Option<&str>) -> anyhow::Result<()> {
    let hooks_dir = gemini_dir.join("hooks");
    let settings_path = gemini_dir.join("settings.json");

    info(&format!("» {}", gemini_dir.display()));

    // Remove legacy shell hook wrappers. Hooks are configured to invoke
    // binaries directly, so shell scripts are no longer required.
    remove_legacy_hook_files(&hooks_dir)?;

    // 2. Configure settings.json
    ensure_parent(&settings_path)?;
    let mut settings = read_json(&settings_path)?;

    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }

    // Use qartez-guard binary directly (no bash dependency)
    let guard_bin_path = find_binary("qartez-guard").map(|p| p.to_string_lossy().into_owned());

    // Use qartez-setup --session-start for session start hook (no bash dependency)
    let setup_bin_path = find_binary("qartez-setup")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "qartez-setup".to_string());
    let session_cmd = format!("{} --session-start", setup_bin_path);

    // BeforeTool: glob|grep_search guard
    if let Some(guard) = guard_bin_path.as_deref() {
        ensure_hook_entry(
            &mut settings,
            "BeforeTool",
            "glob|grep_search",
            "qartez-guard",
            guard,
            3000,
        );
    } else {
        warn("qartez-guard binary not found; glob/grep guard hook skipped");
    }

    // BeforeTool: replace|write_file modification guard
    if let Some(guard) = guard_bin_path.as_deref() {
        ensure_hook_entry(
            &mut settings,
            "BeforeTool",
            "replace|write_file",
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
    if settings.get("mcpServers").is_none() {
        settings["mcpServers"] = serde_json::json!({});
    }
    settings["mcpServers"]["qartez"] = serde_json::json!({
        "command": bin,
        "args": []
    });

    backup_file(&settings_path)?;
    write_json(&settings_path, &settings)?;
    info(&format!("Settings updated: {}", settings_path.display()));

    // 3. Install GEMINI.md snippet
    install_gemini_md_snippet(&gemini_dir.join("GEMINI.md"))?;

    Ok(())
}

fn uninstall_gemini() -> anyhow::Result<()> {
    let dirs = discover_gemini_dirs();
    if dirs.is_empty() {
        return uninstall_gemini_one(&home_dir().join(".gemini"));
    }

    for dir in &dirs {
        uninstall_gemini_one(dir)?;
    }
    info("Uninstall complete");
    Ok(())
}

fn uninstall_gemini_one(gemini_dir: &Path) -> anyhow::Result<()> {
    let hooks_dir = gemini_dir.join("hooks");
    let settings_path = gemini_dir.join("settings.json");

    info(&format!("» {}", gemini_dir.display()));

    for name in [
        "qartez-guard.sh",
        "qartez-session-start.sh",
        "qartez-guard.ps1",
        "qartez-session-start.ps1",
    ] {
        let path = hooks_dir.join(name);
        if path.is_file() {
            fs::remove_file(&path)?;
            info(&format!("Hook removed: {}", path.display()));
        }
    }

    if settings_path.is_file() {
        backup_file(&settings_path)?;
        let mut settings = read_json(&settings_path)?;

        remove_hook_entries_containing(&mut settings, "BeforeTool", "qartez-guard");
        remove_hook_entries_containing(&mut settings, "SessionStart", "qartez-session-start");

        if let Some(hooks) = settings.get("hooks")
            && hooks.as_object().is_some_and(|o| o.is_empty())
        {
            settings.as_object_mut().map(|o| o.remove("hooks"));
        }

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

    remove_gemini_md_snippet(&gemini_dir.join("GEMINI.md"))?;
    Ok(())
}

fn install_gemini_md_snippet(target: &Path) -> anyhow::Result<()> {
    install_snippet(
        target,
        GEMINI_MD_SNIPPET,
        "<!-- qartez-mcp-instructions -->",
        "<!-- /qartez-mcp-instructions -->",
    )
}

fn remove_gemini_md_snippet(target: &Path) -> anyhow::Result<()> {
    remove_snippet(
        target,
        "<!-- qartez-mcp-instructions -->",
        "<!-- /qartez-mcp-instructions -->",
    )
}

// -- Shared snippet helpers --------------------------------------------------

fn install_snippet(target: &Path, snippet: &str, begin: &str, end: &str) -> anyhow::Result<()> {
    ensure_parent(target)?;

    if !target.is_file() {
        fs::write(target, snippet)?;
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
        out.push_str(snippet);
        fs::write(target, out)?;
        info(&format!("Appended qartez snippet to {}", target.display()));
        return Ok(());
    }

    let mut result = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line == begin {
            skipping = true;
            result.push_str(snippet);
            if !snippet.ends_with('\n') {
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

fn remove_snippet(target: &Path, begin: &str, end: &str) -> anyhow::Result<()> {
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
    info(&format!("Qartez snippet removed from {}", target.display()));
    Ok(())
}

/// Install or update the AGENTS.md snippet (for Codex and OpenCode).
fn install_agents_md_snippet(target: &Path) -> anyhow::Result<()> {
    install_snippet(
        target,
        AGENTS_MD_SNIPPET,
        "<!-- qartez-mcp-instructions -->",
        "<!-- /qartez-mcp-instructions -->",
    )
}

fn remove_agents_md_snippet(target: &Path) -> anyhow::Result<()> {
    remove_snippet(
        target,
        "<!-- qartez-mcp-instructions -->",
        "<!-- /qartez-mcp-instructions -->",
    )
}

/// Install the qartez skill into `~/.claude/skills/qartez/`.
///
/// The skill provides on-demand workflow orchestration for qartez MCP tools,
/// replacing the detailed instructions that previously lived in CLAUDE.md.
/// Only the minimal tool-mapping table remains always-loaded; the full
/// workflow guidance loads when the skill triggers.
fn install_skill(claude_dir: &Path) -> anyhow::Result<()> {
    let skill_dir = claude_dir.join("skills").join("qartez");
    let refs_dir = skill_dir.join("references");
    fs::create_dir_all(&refs_dir)?;

    let skill_path = skill_dir.join("SKILL.md");
    let tools_path = refs_dir.join("tools.md");
    let guard_path = refs_dir.join("guard.md");

    fs::write(&skill_path, SKILL_MD)?;
    fs::write(&tools_path, SKILL_TOOLS_MD)?;
    fs::write(&guard_path, SKILL_GUARD_MD)?;

    info(&format!("Skill installed: {}", skill_dir.display()));
    Ok(())
}

/// Remove the qartez skill from `~/.claude/skills/qartez/`.
fn remove_skill(claude_dir: &Path) -> anyhow::Result<()> {
    let skill_dir = claude_dir.join("skills").join("qartez");
    if skill_dir.is_dir() {
        fs::remove_dir_all(&skill_dir)?;
        info(&format!("Skill removed: {}", skill_dir.display()));
    }
    Ok(())
}

/// Install the `.mdc` rule file for Cursor.
fn install_cursor_rule(cursor_dir: &Path) -> anyhow::Result<()> {
    let rules_dir = cursor_dir.join("rules");
    fs::create_dir_all(&rules_dir)?;
    let target = rules_dir.join("qartez.mdc");

    if target.is_file() {
        let current = fs::read_to_string(&target)?;
        if current.trim() == CURSOR_RULE_MDC.trim() {
            info(&format!(
                "Cursor rule already up to date: {}",
                target.display()
            ));
            return Ok(());
        }
        backup_file(&target)?;
    }

    fs::write(&target, CURSOR_RULE_MDC)?;
    info(&format!("Cursor rule installed: {}", target.display()));
    Ok(())
}

fn remove_cursor_rule(cursor_dir: &Path) -> anyhow::Result<()> {
    let target = cursor_dir.join("rules").join("qartez.mdc");
    if target.is_file() {
        fs::remove_file(&target)?;
        info(&format!("Cursor rule removed: {}", target.display()));
    }
    Ok(())
}

/// Install the full instructions as a standalone markdown rules file.
/// Used for IDEs that support a `rules/` directory (Kiro, Windsurf, etc.)
/// but lack a skill mechanism.
fn install_rules_file(rules_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(rules_dir)?;
    let target = rules_dir.join("qartez.md");

    if target.is_file() {
        let current = fs::read_to_string(&target)?;
        if current.trim() == INSTRUCTIONS_MD.trim() {
            info(&format!(
                "Rules file already up to date: {}",
                target.display()
            ));
            return Ok(());
        }
        backup_file(&target)?;
    }

    fs::write(&target, INSTRUCTIONS_MD)?;
    info(&format!("Rules file installed: {}", target.display()));
    Ok(())
}

fn remove_rules_file(rules_dir: &Path) -> anyhow::Result<()> {
    let target = rules_dir.join("qartez.md");
    if target.is_file() {
        fs::remove_file(&target)?;
        info(&format!("Rules file removed: {}", target.display()));
    }
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
        uninstall_claude_one(&home_dir().join(".claude"))?;
    } else {
        for dir in &dirs {
            uninstall_claude_one(dir)?;
        }
    }

    // CLAUDE.md snippet and skill live only in ~/.claude
    let primary = home_dir().join(".claude");
    remove_claude_md_snippet(&primary.join("CLAUDE.md"))?;
    remove_skill(&primary)?;

    info("Uninstall complete");
    Ok(())
}

fn uninstall_claude_one(claude_dir: &Path) -> anyhow::Result<()> {
    let hooks_dir = claude_dir.join("hooks");
    let settings_path = claude_dir.join("settings.json");

    info(&format!("» {}", claude_dir.display()));

    // Remove hook files
    for name in [
        "qartez-guard.sh",
        "qartez-session-start.sh",
        "qartez-guard.ps1",
        "qartez-session-start.ps1",
    ] {
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

    // Mirror the cleanup into Claude Code's runtime state file.
    let state_path = claude_state_file(claude_dir);
    if state_path.is_file() {
        backup_file(&state_path)?;
        let mut state = read_json(&state_path)?;
        if let Some(servers) = state.get_mut("mcpServers")
            && let Some(obj) = servers.as_object_mut()
        {
            obj.remove("qartez");
        }
        write_json(&state_path, &state)?;
        info(&format!("State cleaned up: {}", state_path.display()));
    }

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

fn remove_legacy_hook_files(hooks_dir: &Path) -> anyhow::Result<()> {
    for name in [
        "qartez-guard.sh",
        "qartez-session-start.sh",
        "qartez-guard.ps1",
        "qartez-session-start.ps1",
    ] {
        let path = hooks_dir.join(name);
        if path.is_file() {
            fs::remove_file(&path)?;
            info(&format!("Legacy hook removed: {}", path.display()));
        }
    }
    Ok(())
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
        info(&format!(
            "Added qartez to {ide} config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Updated qartez in {ide} config: {} -> {bin}",
            current.unwrap_or_default()
        ));
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
    let json_str = if cleaned.trim().is_empty() {
        "{}"
    } else {
        &cleaned
    };
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
    let existed = data["context_servers"].get("qartez").is_some();
    data["context_servers"]["qartez"] = desired;

    // Write back as clean JSON (comments stripped, but that's acceptable)
    write_json(&config_path, &data)?;
    if existed {
        info(&format!(
            "Updated qartez in Zed config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Added qartez to Zed config: {}",
            config_path.display()
        ));
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
        .ok_or_else(|| anyhow::anyhow!("YAML config is not a mapping"))?;

    // Ensure mcpServers is a sequence
    let servers_key = serde_yaml::Value::String("mcpServers".into());
    if !mapping.contains_key(&servers_key) || !mapping[&servers_key].is_sequence() {
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
        .ok_or_else(|| anyhow::anyhow!("YAML config is not a mapping"))?;

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
    info(&format!("Removed qartez from {}", config_path.display()));
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
    info(&format!("Removed qartez from {}", config_path.display()));
    Ok(())
}

// -- Copilot CLI (JSON with `servers` key instead of `mcpServers`) -----------

fn install_copilot_cli(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::CopilotCli.config_path();
    ensure_parent(&config_path)?;

    let mut data = read_json(&config_path)?;
    if data.get("servers").is_none() {
        data["servers"] = serde_json::json!({});
    }

    let current = data["servers"]["qartez"]["command"]
        .as_str()
        .map(str::to_string);
    if current.as_deref() == Some(bin) {
        info(&format!(
            "Copilot CLI already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    data["servers"]["qartez"] = serde_json::json!({
        "type": "stdio",
        "command": bin,
        "args": []
    });
    write_json(&config_path, &data)?;

    if current.is_none() {
        info(&format!(
            "Added qartez to Copilot CLI config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Updated qartez in Copilot CLI config: {} -> {bin}",
            current.unwrap_or_default()
        ));
    }
    Ok(())
}

fn uninstall_copilot_cli() -> anyhow::Result<()> {
    let config_path = Ide::CopilotCli.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Copilot CLI config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let mut data = read_json(&config_path)?;
    let present = data.get("servers").and_then(|s| s.get("qartez")).is_some();
    if !present {
        info(&format!(
            "qartez not present in {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    if let Some(servers) = data.get_mut("servers").and_then(|s| s.as_object_mut()) {
        servers.remove("qartez");
    }
    write_json(&config_path, &data)?;
    info(&format!("Removed qartez from {}", config_path.display()));
    Ok(())
}

// -- Amp (JSON with `amp.mcpServers` key) ------------------------------------

fn install_amp(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::Amp.config_path();
    ensure_parent(&config_path)?;

    let mut data = read_json(&config_path)?;
    if data.get("amp.mcpServers").is_none() {
        data["amp.mcpServers"] = serde_json::json!({});
    }

    let current = data["amp.mcpServers"]["qartez"]["command"]
        .as_str()
        .map(str::to_string);
    if current.as_deref() == Some(bin) {
        info(&format!(
            "Amp already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    backup_file(&config_path)?;
    data["amp.mcpServers"]["qartez"] = serde_json::json!({
        "command": bin,
        "args": []
    });
    write_json(&config_path, &data)?;

    if current.is_none() {
        info(&format!(
            "Added qartez to Amp config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Updated qartez in Amp config: {} -> {bin}",
            current.unwrap_or_default()
        ));
    }
    Ok(())
}

fn uninstall_amp() -> anyhow::Result<()> {
    let config_path = Ide::Amp.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Amp config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let mut data = read_json(&config_path)?;
    let present = data
        .get("amp.mcpServers")
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
        .get_mut("amp.mcpServers")
        .and_then(|s| s.as_object_mut())
    {
        servers.remove("qartez");
    }
    write_json(&config_path, &data)?;
    info(&format!("Removed qartez from {}", config_path.display()));
    Ok(())
}

// -- Augment (JSON with `mcpServers` in settings.json) -----------------------

fn install_augment(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::Augment.config_path();
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
            "Augment already has qartez pointing at {bin} (no changes)."
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
        info(&format!(
            "Added qartez to Augment config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Updated qartez in Augment config: {} -> {bin}",
            current.unwrap_or_default()
        ));
    }
    Ok(())
}

fn uninstall_augment() -> anyhow::Result<()> {
    let config_path = Ide::Augment.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Augment config found at {}. Nothing to uninstall.",
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

// -- Goose (YAML with `extensions` key) --------------------------------------

fn install_goose(bin: &str) -> anyhow::Result<()> {
    let config_path = Ide::Goose.config_path();
    ensure_parent(&config_path)?;

    let mut data: serde_yaml::Value = if config_path.is_file() {
        let text = fs::read_to_string(&config_path)?;
        serde_yaml::from_str(&text).unwrap_or(serde_yaml::Value::Mapping(Default::default()))
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };

    let mapping = data
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("YAML config is not a mapping"))?;

    let ext_key = serde_yaml::Value::String("extensions".into());
    if !mapping.contains_key(&ext_key) || !mapping[&ext_key].is_mapping() {
        mapping.insert(
            ext_key.clone(),
            serde_yaml::Value::Mapping(Default::default()),
        );
    }

    let extensions = mapping[&ext_key]
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("Goose 'extensions' field is not a mapping"))?;

    let qartez_key = serde_yaml::Value::String("qartez".into());

    let desired = {
        let mut m = serde_yaml::Mapping::new();
        m.insert("command".into(), serde_yaml::Value::String(bin.into()));
        m.insert("args".into(), serde_yaml::Value::Sequence(vec![]));
        serde_yaml::Value::Mapping(m)
    };

    if extensions.get(&qartez_key) == Some(&desired) {
        info(&format!(
            "Goose already has qartez pointing at {bin} (no changes)."
        ));
        return Ok(());
    }

    let existed = extensions.contains_key(&qartez_key);
    backup_file(&config_path)?;
    extensions.insert(qartez_key, desired);

    let out = serde_yaml::to_string(&data)?;
    fs::write(&config_path, out)?;

    if existed {
        info(&format!(
            "Updated qartez in Goose config: {}",
            config_path.display()
        ));
    } else {
        info(&format!(
            "Added qartez to Goose config: {}",
            config_path.display()
        ));
    }
    Ok(())
}

fn uninstall_goose() -> anyhow::Result<()> {
    let config_path = Ide::Goose.config_path();
    if !config_path.is_file() {
        info(&format!(
            "No Goose config found at {}. Nothing to uninstall.",
            config_path.display()
        ));
        return Ok(());
    }

    let text = fs::read_to_string(&config_path)?;
    let mut data: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let mapping = data
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("YAML config is not a mapping"))?;

    let ext_key = serde_yaml::Value::String("extensions".into());
    let qartez_key = serde_yaml::Value::String("qartez".into());

    let Some(extensions) = mapping.get_mut(&ext_key).and_then(|v| v.as_mapping_mut()) else {
        info("qartez not present in Goose config. Nothing to uninstall.");
        return Ok(());
    };

    if !extensions.contains_key(&qartez_key) {
        info("qartez not present in Goose config. Nothing to uninstall.");
        return Ok(());
    }

    backup_file(&config_path)?;
    extensions.remove(&qartez_key);
    let out = serde_yaml::to_string(&data)?;
    fs::write(&config_path, out)?;
    info(&format!("Removed qartez from {}", config_path.display()));
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
    info(&format!("Removed qartez from {}", config_path.display()));
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

    if cli.session_start {
        return run_session_start();
    }

    if cli.update || cli.update_background {
        return run_update(cli.update_background);
    }

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

    // Global gitignore applies regardless of which IDE was selected -
    // .qartez/ directories are created for any IDE, not just Claude Code.
    if let Err(e) = install_global_gitignore() {
        eprintln!(
            "  {} Failed to update global gitignore: {e:#}",
            style("[!]").yellow()
        );
    }

    // Download the semantic search model when compiled with the feature.
    #[cfg(feature = "semantic")]
    if let Err(e) = download_semantic_model() {
        eprintln!(
            "  {} Failed to download semantic model: {e:#}",
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

// -- Semantic model download --------------------------------------------------

/// Download the Jina Code v2 ONNX model and tokenizer for semantic search.
///
/// Files are stored in `~/.qartez/models/jina-embeddings-v2-base-code/`.
/// Skips the download if the files already exist.
#[cfg(feature = "semantic")]
fn download_semantic_model() -> anyhow::Result<()> {
    let model_dir = home_dir()
        .join(".qartez")
        .join("models")
        .join("jina-embeddings-v2-base-code");

    let model_path = model_dir.join("model.onnx");
    let tokenizer_path = model_dir.join("tokenizer.json");

    if model_path.exists() && tokenizer_path.exists() {
        eprintln!(
            "  {} Semantic model already downloaded at {}",
            style("[=]").cyan(),
            model_dir.display()
        );
        return Ok(());
    }

    fs::create_dir_all(&model_dir)?;

    let base_url = "https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/main";

    let files = [
        ("model.onnx", &model_path),
        ("tokenizer.json", &tokenizer_path),
    ];

    for (filename, dest) in &files {
        if dest.exists() {
            continue;
        }
        let url = format!("{base_url}/{filename}");
        eprintln!("  {} Downloading {filename}...", style("[>]").cyan(),);
        let status = Command::new("curl")
            .args(["-fSL", "--progress-bar", "-o"])
            .arg(dest.as_os_str())
            .arg(&url)
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            anyhow::bail!("curl failed to download {url}");
        }
    }

    eprintln!(
        "  {} Semantic model ready at {}",
        style("[=]").green(),
        model_dir.display()
    );
    Ok(())
}

// -- Session-start hook (Rust equivalent of qartez-session-start.sh) ----------

/// Repo markers that indicate this is a real code project (not a random folder).
const REPO_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
];

/// Rust implementation of the session-start hook behavior.
/// Mirrors `scripts/qartez-session-start.sh` but requires no bash.
fn run_session_start() -> anyhow::Result<()> {
    let project_dir = std::env::var("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());

    if project_dir.as_os_str().is_empty() || !project_dir.is_dir() {
        return Ok(());
    }

    // Skip dangerous roots
    let home = home_dir();
    if project_dir == home || project_dir == PathBuf::from("/") {
        return Ok(());
    }

    // Already indexed
    if project_dir.join(".qartez").is_dir() {
        return Ok(());
    }

    // Require at least one repo marker
    let has_marker = REPO_MARKERS.iter().any(|m| project_dir.join(m).exists());
    if !has_marker {
        return Ok(());
    }

    // Locate qartez-mcp binary
    let binary = find_binary("qartez-mcp").or_else(|| {
        // Fallback: check QARTEZ_BINARY env var
        std::env::var("QARTEZ_BINARY")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    });

    let Some(binary) = binary else {
        return Ok(());
    };

    // Spawn detached background reindex
    let log_dir = home.join(".cache").join("qartez-mcp");
    let _ = fs::create_dir_all(&log_dir);
    let log_file = log_dir.join("session-index.log");

    #[cfg(unix)]
    {
        let _ = std::process::Command::new(&binary)
            .arg("--root")
            .arg(&project_dir)
            .arg("--reindex")
            .stdout(fs::OpenOptions::new().create(true).append(true).open(&log_file)?)
            .stderr(fs::OpenOptions::new().create(true).append(true).open(&log_file)?)
            .stdin(Stdio::null())
            .spawn();
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED: u32 = 0x00000008;
        let mut cmd = std::process::Command::new(&binary);
        cmd.arg("--root")
            .arg(&project_dir)
            .arg("--reindex")
            .stdout(fs::OpenOptions::new().create(true).append(true).open(&log_file)?)
            .stderr(fs::OpenOptions::new().create(true).append(true).open(&log_file)?)
            .stdin(Stdio::null())
            .creation_flags(DETACHED);
        cmd.spawn().ok();
    }

    Ok(())
}

// -- Auto-update -------------------------------------------------------------

const QARTEZ_UPDATE_REPO: &str = "kuberstar/qartez-mcp";
#[cfg(unix)]
const QARTEZ_INSTALL_URL: &str =
    "https://raw.githubusercontent.com/kuberstar/qartez-mcp/main/install.sh";
const UPDATE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

fn update_cache_path() -> PathBuf {
    home_dir().join(".qartez").join("last-update-check")
}

fn touch_update_cache() {
    let path = update_cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, "");
}

fn update_cache_is_fresh() -> bool {
    let Ok(meta) = fs::metadata(update_cache_path()) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age < UPDATE_TTL)
        .unwrap_or(false)
}

// Cross-process exclusion for the update path. Users frequently open many
// Claude Code sessions at once - each starts qartez-mcp which spawns
// qartez-setup --update-background. Without a lock, all of them would
// race to the GitHub API and potentially to parallel install.sh rebuilds.
//
// Uses advisory flock on ~/.qartez/update.lock. The OS releases it on
// process exit, so a crashed updater won't leave a dangling lock. The
// returned file handle must be kept alive for the duration of the
// critical section - dropping it releases the lock.
fn acquire_update_lock() -> Option<fs::File> {
    use fs4::fs_std::FileExt;
    let path = home_dir().join(".qartez").join("update.lock");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    match file.try_lock_exclusive() {
        Ok(true) => Some(file),
        Ok(false) | Err(_) => None,
    }
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split('-').next().unwrap_or(s);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn fetch_latest_release_tag() -> anyhow::Result<String> {
    let url = format!("https://api.github.com/repos/{QARTEZ_UPDATE_REPO}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-sSfL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: qartez-setup",
            &url,
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to invoke curl: {e}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "GitHub API request failed (curl exit {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("invalid JSON from GitHub API: {e}"))?;
    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("`tag_name` missing from GitHub API response"))
}

fn run_update(background: bool) -> anyhow::Result<()> {
    if std::env::var_os("QARTEZ_NO_AUTO_UPDATE").is_some() {
        if !background {
            eprintln!("  Auto-update disabled via QARTEZ_NO_AUTO_UPDATE.");
        }
        return Ok(());
    }

    if background && update_cache_is_fresh() {
        return Ok(());
    }

    // Cross-process lock - only one qartez-setup update may run at a
    // time across all Claude Code sessions. Other parallel invocations
    // skip silently; by the time they check the TTL on their next start,
    // the winning process will have touched the cache.
    //
    // The cache touch lives inside the lock critical section (on success
    // paths) so transient network failures still leave the cache stale
    // and the next startup can retry.
    let _lock = match acquire_update_lock() {
        Some(f) => f,
        None => {
            if !background {
                eprintln!("  Another qartez-setup update is already running - skipping.");
            }
            return Ok(());
        }
    };

    let current = env!("CARGO_PKG_VERSION");
    let latest = match fetch_latest_release_tag() {
        Ok(tag) => tag,
        Err(e) => {
            if background {
                return Ok(());
            }
            return Err(e);
        }
    };

    if !is_newer_version(&latest, current) {
        touch_update_cache();
        if !background {
            eprintln!("  Already on the latest version (v{current}).");
        }
        return Ok(());
    }

    eprintln!(
        "  {} qartez {} → {} - rebuilding from source...",
        style("[+]").green(),
        current,
        latest.trim_start_matches('v'),
    );

#[cfg(unix)]
    {
        let install_cmd = format!("curl -sSfL {QARTEZ_INSTALL_URL} | sh");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&install_cmd)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn installer: {e}"))?;
        let status = child
            .wait()
            .map_err(|e| anyhow::anyhow!("installer wait failed: {e}"))?;

        if !status.success() {
            anyhow::bail!(
                "installer exited with status {}",
                status.code().unwrap_or(-1)
            );
        }

        touch_update_cache();
        eprintln!(
            " {} Update complete: {} → {}",
            style("✓").green().bold(),
            current,
            latest.trim_start_matches('v'),
        );
    }

    #[cfg(windows)]
    {
        eprintln!(
            " {} Auto-update on Windows requires WSL or Git Bash.",
            style("[!]").yellow()
        );
        eprintln!(
            " {} Visit https://github.com/kuberstar/qartez-mcp/releases to download v{} manually.",
            style("[i]").cyan(),
            latest.trim_start_matches('v'),
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
    eprintln!("  {} Uninstall complete.", style("✓").green().bold());
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
                    Ide::Gemini => {
                        let dirs = discover_gemini_dirs();
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
                format!("{:<16} ({})", ide.to_string(), style("not installed").dim())
            }
        })
        .collect();

    let defaults: Vec<bool> = detected.iter().map(|(_, det)| *det).collect();

    let selections = MultiSelect::new()
        .with_prompt("  Select IDEs to configure (Space to toggle, Enter to confirm)")
        .items(&items)
        .defaults(&defaults)
        .interact()?;

    Ok(selections.into_iter().map(|i| detected[i].0).collect())
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
            anyhow::bail!("Unknown IDE '{name}'. Known: {}", known.join(", "));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_strips_v_prefix_and_pre_release() {
        assert_eq!(parse_semver("0.1.1"), Some((0, 1, 1)));
        assert_eq!(parse_semver("v0.1.1"), Some((0, 1, 1)));
        assert_eq!(parse_semver("v1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_semver(" v0.1.1 "), Some((0, 1, 1)));
    }

    #[test]
    fn parse_semver_rejects_malformed() {
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("v1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("v1.x.0"), None);
    }

    #[test]
    fn is_newer_uses_numeric_component_order() {
        // Lexical compare would say "0.1.2" > "0.1.10" - make sure we don't.
        assert!(is_newer_version("0.1.10", "0.1.2"));
        assert!(is_newer_version("v0.2.0", "v0.1.99"));
        assert!(is_newer_version("v1.0.0", "v0.99.99"));
    }

    #[test]
    fn is_newer_returns_false_for_equal_or_older_or_unparseable() {
        assert!(!is_newer_version("0.1.1", "0.1.1"));
        assert!(!is_newer_version("v0.1.0", "v0.1.1"));
        assert!(!is_newer_version("garbage", "0.1.1"));
        assert!(!is_newer_version("0.1.1", "garbage"));
        assert!(!is_newer_version("garbage", "garbage"));
    }

    // -- Test helpers for env-var–dependent code ----------------------------

    // Serializes tests that mutate process-global env vars ($HOME, $PATH).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        match ENV_LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: all env-mutating tests are serialized by ENV_LOCK.
            unsafe { std::env::set_var(key, val.as_ref()) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: same serialization guarantee as set().
            match &self.original {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    // -- Cache TTL ---------------------------------------------------------

    #[test]
    fn update_cache_missing_is_not_fresh() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());
        assert!(!update_cache_is_fresh());
    }

    #[test]
    fn update_cache_fresh_after_touch() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());
        touch_update_cache();
        assert!(update_cache_is_fresh());
    }

    #[test]
    fn update_cache_stale_when_mtime_exceeds_ttl() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());
        touch_update_cache();
        let path = update_cache_path();
        let old = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        let times = fs::FileTimes::new().set_modified(old);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(times)
            .unwrap();
        assert!(!update_cache_is_fresh());
    }

    // -- Lock behavior -----------------------------------------------------

    #[test]
    fn acquire_lock_succeeds_and_creates_file() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());
        let lock = acquire_update_lock();
        assert!(lock.is_some());
        assert!(tmp.path().join(".qartez").join("update.lock").exists());
    }

    #[test]
    #[cfg(unix)]
    fn acquire_lock_returns_none_for_unwritable_lock_file() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());
        let dir = tmp.path().join(".qartez");
        fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("update.lock");
        fs::write(&lock_path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o444)).unwrap();
        }
        let result = acquire_update_lock();
        assert!(result.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    #[test]
    fn lock_released_on_drop_allows_reacquire() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());
        {
            let _lock = acquire_update_lock().unwrap();
        }
        let lock2 = acquire_update_lock();
        assert!(lock2.is_some(), "reacquire after drop should succeed");
    }

    // -- Semver edge cases -------------------------------------------------

    #[test]
    fn parse_semver_handles_large_and_zero_components() {
        assert_eq!(parse_semver("0.0.0"), Some((0, 0, 0)));
        assert_eq!(parse_semver("999.999.999"), Some((999, 999, 999)));
        assert_eq!(parse_semver("v100.0.0-beta.1"), Some((100, 0, 0)));
    }

    #[test]
    fn parse_semver_rejects_overflow_and_edge_cases() {
        assert_eq!(parse_semver("4294967296.0.0"), None);
        assert_eq!(parse_semver("v"), None);
        assert_eq!(parse_semver("1.2."), None);
        assert_eq!(parse_semver(".1.2"), None);
        assert_eq!(parse_semver("1..2"), None);
    }

    #[test]
    fn is_newer_prerelease_compared_by_core_version() {
        assert!(!is_newer_version("v1.0.0-rc1", "1.0.0"));
        assert!(is_newer_version("v1.0.1-beta", "1.0.0"));
    }

    #[test]
    fn install_claude_removes_legacy_shell_hooks_and_uses_binary_session_start() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());

        let claude_dir = tmp.path().join(".claude");
        let hooks = claude_dir.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(hooks.join("qartez-guard.sh"), "#!/bin/sh\n").unwrap();
        fs::write(hooks.join("qartez-session-start.sh"), "#!/bin/sh\n").unwrap();

        install_claude_one(&claude_dir, "qartez-mcp", None).unwrap();

        assert!(!hooks.join("qartez-guard.sh").exists());
        assert!(!hooks.join("qartez-session-start.sh").exists());

        let settings = read_json(&claude_dir.join("settings.json")).unwrap();
        let session_cmd = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(session_cmd.contains("qartez-setup"));
        assert!(session_cmd.contains("--session-start"));
        assert!(!session_cmd.contains(".sh"));
    }

    #[test]
    fn install_gemini_removes_legacy_shell_hooks_and_uses_binary_session_start() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", tmp.path());

        let gemini_dir = tmp.path().join(".gemini");
        let hooks = gemini_dir.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(hooks.join("qartez-guard.sh"), "#!/bin/sh\n").unwrap();
        fs::write(hooks.join("qartez-session-start.sh"), "#!/bin/sh\n").unwrap();

        install_gemini_one(&gemini_dir, "qartez-mcp", None).unwrap();

        assert!(!hooks.join("qartez-guard.sh").exists());
        assert!(!hooks.join("qartez-session-start.sh").exists());

        let settings = read_json(&gemini_dir.join("settings.json")).unwrap();
        let session_cmd = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(session_cmd.contains("qartez-setup"));
        assert!(session_cmd.contains("--session-start"));
        assert!(!session_cmd.contains(".sh"));
    }

    // -- Mock curl for fetch_latest_release_tag ----------------------------

    #[cfg(unix)]
    fn write_mock_curl(dir: &Path, body: &str) {
        let mock = dir.join("curl");
        fs::write(&mock, format!("#!/bin/sh\necho '{body}'")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mock, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn write_mock_curl_failing(dir: &Path, code: i32) {
        let mock = dir.join("curl");
        fs::write(&mock, format!("#!/bin/sh\nexit {code}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&mock, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn fetch_release_tag_parses_valid_github_response() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        write_mock_curl(tmp.path(), r#"{"tag_name": "v0.2.0"}"#);
        let orig = std::env::var("PATH").unwrap_or_default();
        let _path = EnvGuard::set("PATH", format!("{}:{orig}", tmp.path().display()));
        assert_eq!(fetch_latest_release_tag().unwrap(), "v0.2.0");
    }

    #[test]
    #[cfg(unix)]
    fn fetch_release_tag_rejects_invalid_json() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        write_mock_curl(tmp.path(), "not json");
        let orig = std::env::var("PATH").unwrap_or_default();
        let _path = EnvGuard::set("PATH", format!("{}:{orig}", tmp.path().display()));
        assert!(fetch_latest_release_tag().is_err());
    }

    #[test]
    #[cfg(unix)]
    fn fetch_release_tag_rejects_missing_tag_name() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        write_mock_curl(tmp.path(), r#"{"name": "Release"}"#);
        let orig = std::env::var("PATH").unwrap_or_default();
        let _path = EnvGuard::set("PATH", format!("{}:{orig}", tmp.path().display()));
        assert!(fetch_latest_release_tag().is_err());
    }

    #[test]
    #[cfg(unix)]
    fn fetch_release_tag_returns_error_on_curl_failure() {
        let _mu = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        write_mock_curl_failing(tmp.path(), 22);
        let orig = std::env::var("PATH").unwrap_or_default();
        let _path = EnvGuard::set("PATH", format!("{}:{orig}", tmp.path().display()));
        assert!(fetch_latest_release_tag().is_err());
    }
}
