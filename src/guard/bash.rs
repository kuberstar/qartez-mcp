//! Shell-exploration guard: steers an agent from `grep`/`rg`, `sed -n`,
//! `cat` and `find` over source code to the qartez tools.
//!
//! Current Claude Code builds have no Grep/Glob tools: every code search and
//! read goes through `Bash`, so a guard keyed on Grep/Glob never fires. A
//! soft `additionalContext` reminder does not work either, because Claude
//! Code delivers it together with the command's result, after the shell has
//! already answered. This module therefore counts consecutive shell reads
//! and searches of code files inside the project and, at a threshold,
//! denies the call with the exact qartez equivalent. The block holds - a plain
//! retry or the next code read is denied too - until a qartez call, an
//! explicit [`ACK_VAR`] prefix (the shell really is the right tool), three
//! denials in a row, or [`BLOCK_EXPIRE_SECS`]. An ack or a give-up starts the
//! cooldown.
//!
//! The first version backed off for the cooldown right after a single
//! denial, and its message said "run it again": in real sessions 21% of
//! denials were followed by the identical command and 34% by another shell
//! read, against 39% by a qartez call.
//!
//! The classification is deliberately narrow, after the false positives
//! Serena's reminder hook hit in the field: a `grep` that only filters a
//! pipe, a read or search of Markdown/config/log files, paths outside the
//! project, and vendored or build directories never count. A command that
//! also does other work (writes a file, installs, builds) is never denied,
//! because a deny skips the whole call: it gets the reminder as context
//! instead.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Consecutive code reads/searches before the guard steps in.
pub const DEFAULT_STREAK: u32 = 3;
/// Quiet period after an acknowledged shell read or a give-up.
pub const DEFAULT_COOLDOWN_SECS: u64 = 120;
/// Prefix that lets a denied command through: `QARTEZ_SHELL=1 grep ...`.
pub const ACK_VAR: &str = "QARTEZ_SHELL";
/// Denials in a row (no qartez call, no ack) after which the guard gives up,
/// so a model that cannot follow the advice is not blocked forever.
pub const MAX_BLOCKS: u32 = 3;
/// A block left unresolved for this long is dropped.
pub const BLOCK_EXPIRE_SECS: u64 = 900;

/// Source-file extensions qartez answers better than the shell. Config,
/// markup and docs are left out on purpose: a key lookup in a TOML file is
/// what `grep` is for.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "java", "kt",
    "kts", "swift", "rb", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "cs", "php", "scala",
    "sc", "vue", "svelte", "dart", "lua", "zig", "ex", "exs", "erl", "hs", "ml", "mli", "clj",
    "cljs", "jl", "m", "mm", "sol", "nim", "groovy", "fs", "fsx",
];

/// `rg -t` type names that select source code.
const CODE_RG_TYPES: &[&str] = &[
    "rust",
    "ts",
    "typescript",
    "js",
    "py",
    "python",
    "go",
    "java",
    "kotlin",
    "swift",
    "ruby",
    "c",
    "cpp",
    "csharp",
    "php",
    "scala",
    "vue",
    "svelte",
    "dart",
    "lua",
    "zig",
    "elixir",
    "haskell",
];

/// Directories whose contents are not the project's own code.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "site-packages",
    "__pycache__",
    ".qartez",
    ".next",
    ".svelte-kit",
];

/// Programs that only read. A command whose every program is here (and
/// that redirects nothing into a file) is safe to deny: nothing is lost.
const READ_ONLY_PROGRAMS: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "ag", "ack", "ugrep", "cat", "head", "tail", "sed", "nl",
    "bat", "less", "more", "awk", "gawk", "find", "ls", "wc", "sort", "uniq", "cut", "tr", "echo",
    "printf", "cd", "pwd", "true", "column", "jq", "file", "stat", "basename", "dirname",
    "realpath", "tree",
];

/// Read-only `git` subcommands.
const READ_ONLY_GIT: &[&str] = &[
    "log",
    "show",
    "diff",
    "status",
    "grep",
    "blame",
    "ls-files",
    "rev-parse",
];

/// One shell operation over project source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exploration {
    /// `grep`/`rg` over code; `pattern` is the first search pattern.
    Search { pattern: Option<String> },
    /// `cat`/`sed -n`/`head`/... of a code file, project-relative.
    Read {
        path: String,
        start: Option<u32>,
        end: Option<u32>,
    },
    /// `find -name '*.rs'` and friends.
    Find,
}

/// What a shell command does, as far as the guard cares.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Classified {
    pub explorations: Vec<Exploration>,
    /// No program outside `READ_ONLY_PROGRAMS`, no file redirect, no
    /// command substitution: denying it loses nothing.
    pub read_only: bool,
    /// The command carries an [`ACK_VAR`]`=...` assignment.
    pub acked: bool,
}

/// Per-session streak, persisted between hook invocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StreakState {
    pub streak: u32,
    pub last_nudge_unix: u64,
    /// Denials since the block began; 0 means no block is active.
    #[serde(default)]
    pub blocks: u32,
    /// When the active block began.
    #[serde(default)]
    pub blocked_unix: u64,
}

/// What the hook should answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Let the command run and add a reminder to the model's context.
    Context(String),
    /// Block the command and tell the model what to call instead.
    Deny(String),
}

/// Thresholds, overridable via `QARTEZ_GUARD_BASH_STREAK` (0 turns the
/// shell guard off) and `QARTEZ_GUARD_BASH_COOLDOWN_SECS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashGuardConfig {
    pub streak: u32,
    pub cooldown_secs: u64,
}

impl Default for BashGuardConfig {
    fn default() -> Self {
        Self {
            streak: DEFAULT_STREAK,
            cooldown_secs: DEFAULT_COOLDOWN_SECS,
        }
    }
}

impl BashGuardConfig {
    /// Defaults overridden by valid `QARTEZ_GUARD_BASH_*` env values.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Some(v) = env_parse::<u32>("QARTEZ_GUARD_BASH_STREAK") {
            cfg.streak = v;
        }
        if let Some(v) = env_parse::<u64>("QARTEZ_GUARD_BASH_COOLDOWN_SECS") {
            cfg.cooldown_secs = v;
        }
        cfg
    }
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok()?.trim().parse().ok()
}

/// Advance the streak for one classified command and decide the answer.
///
/// Commands that explore no code leave the streak untouched; only a qartez
/// call resets it (see [`StreakState::reset`]). At the threshold, outside
/// the cooldown, a read-only command is denied and a block begins; a
/// side-effecting one gets the advice as context and starts the cooldown.
pub fn decide(
    state: &mut StreakState,
    cmd: &Classified,
    now_unix: u64,
    cfg: BashGuardConfig,
) -> Verdict {
    if cfg.streak == 0 || cmd.explorations.is_empty() {
        return Verdict::Allow;
    }
    if state.blocks > 0 && now_unix.saturating_sub(state.blocked_unix) >= BLOCK_EXPIRE_SECS {
        state.end_block();
    }
    if cmd.acked {
        state.end_block();
        state.streak = 0;
        state.last_nudge_unix = now_unix;
        return Verdict::Allow;
    }
    if state.blocks > 0 {
        if !cmd.read_only {
            return Verdict::Allow;
        }
        if state.blocks >= MAX_BLOCKS {
            state.end_block();
            state.streak = 0;
            state.last_nudge_unix = now_unix;
            return Verdict::Allow;
        }
        state.blocks += 1;
        return Verdict::Deny(format!(
            "Still blocked: qartez has this code indexed. Use {advice}. The shell command was NOT run. \
             Only if the shell really is the right tool (a non-code file, filtering command output, a file \
             outside the project), re-run it prefixed with `{ACK_VAR}=1 `; a plain retry is denied again.",
            advice = advice(&cmd.explorations),
        ));
    }
    state.streak = state.streak.saturating_add(1);
    let cooled = now_unix.saturating_sub(state.last_nudge_unix) >= cfg.cooldown_secs;
    if state.streak < cfg.streak || !cooled {
        return Verdict::Allow;
    }
    state.streak = 0;
    let advice = advice(&cmd.explorations);
    if cmd.read_only {
        state.blocks = 1;
        state.blocked_unix = now_unix;
        Verdict::Deny(format!(
            "qartez has this code indexed, and this is shell read/search number {n} of source code in a row. \
             Instead: {advice}. The shell command was NOT run, and a plain retry or the next shell read of \
             code is denied too. Only if the shell really is the right tool (a non-code file, filtering \
             command output, a file outside the project), re-run it prefixed with `{ACK_VAR}=1 `.",
            n = cfg.streak,
        ))
    } else {
        state.last_nudge_unix = now_unix;
        Verdict::Context(format!(
            "qartez has this code indexed: for the next read or search of source code use {advice} \
             instead of the shell; it returns the exact symbol for far fewer tokens."
        ))
    }
}

impl StreakState {
    /// A qartez tool call ends the streak and any block.
    pub fn reset(&mut self) {
        self.streak = 0;
        self.end_block();
    }

    fn end_block(&mut self) {
        self.blocks = 0;
        self.blocked_unix = 0;
    }
}

fn advice(explorations: &[Exploration]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for e in explorations {
        let line = match e {
            Exploration::Search { pattern: Some(p) } if is_plain_identifier(p) => format!(
                "`qartez_find name=\"{p}\"` for a definition, `qartez_grep query=\"{p}\"` for symbols, \
                 or `qartez_grep query=\"{p}\" search_bodies=true` for text inside code"
            ),
            Exploration::Search { pattern: Some(p) } => {
                // A regex or phrase: name a body search for its first word
                // instead of echoing a pattern no qartez tool would accept.
                let word = first_word(p).unwrap_or_default();
                format!(
                    "`qartez_grep query=\"{word}\" search_bodies=true` for text inside code \
                     (one term per query; `qartez_grep regex=true` matches symbol names)"
                )
            }
            Exploration::Search { pattern: None } => {
                "`qartez_grep` (symbols, or `search_bodies=true` for text inside code)".to_owned()
            }
            Exploration::Read {
                path,
                start: Some(s),
                end: Some(e),
            } if e >= s => format!(
                "`qartez_read file_path=\"{path}\" start_line={s} limit={}` (or `qartez_outline file_path=\"{path}\"` then `qartez_read symbol_name=...`)",
                e - s + 1
            ),
            Exploration::Read { path, .. } => format!(
                "`qartez_outline file_path=\"{path}\"` then `qartez_read symbol_name=...` (or `qartez_read file_path=\"{path}\"`)"
            ),
            Exploration::Find => "`qartez_map` for the file layout".to_owned(),
        };
        if !parts.contains(&line) {
            parts.push(line);
        }
    }
    parts.join("; ")
}

/// `foo`, `Foo::bar`, `foo.bar`: safe to pass as a symbol name.
fn is_plain_identifier(p: &str) -> bool {
    let mut chars = p.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.'))
}

/// The first run of letters, digits or underscores in a pattern, after
/// dropping regex escapes.
fn first_word(p: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
            if !out.is_empty() {
                break;
            }
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.is_empty() {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// True when the "project" is the user's home directory. An index at
/// `~/.qartez` makes every path under home look like project code, so a
/// `grep` through `~/Library` would count; the shell guard stays out.
pub fn is_home_root(root: &Path, home: Option<&Path>) -> bool {
    home.is_some_and(|h| {
        let h = h.canonicalize().unwrap_or_else(|_| h.to_path_buf());
        h == root
    })
}

/// File the streak for one session (and subagent) lives in.
pub fn state_path(
    project_root: &Path,
    session_id: Option<&str>,
    agent_id: Option<&str>,
) -> PathBuf {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(80)
            .collect()
    };
    let mut key = clean(session_id.unwrap_or("default"));
    if key.is_empty() {
        key.push_str("default");
    }
    if let Some(a) = agent_id.map(clean).filter(|a| !a.is_empty()) {
        key.push('-');
        key.push_str(&a);
    }
    project_root
        .join(".qartez")
        .join("guard")
        .join(format!("bash-{key}.json"))
}

/// Load a streak; a missing or corrupt file is a fresh streak.
pub fn load_state(path: &Path) -> StreakState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist a streak. Best effort: the guard fails open.
pub fn save_state(path: &Path, state: StreakState) {
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return;
    }
    if let Ok(json) = serde_json::to_string(&state) {
        let _ = std::fs::write(path, json);
    }
}

// ---------------------------------------------------------------------------
// Command classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// `&&`, `||`, `;`, `&`, newline: separates commands.
    Sep,
    /// `|`: separates pipeline stages.
    Pipe,
    /// A redirect that writes a file (`>`, `>>`, `&>`, `N>` to a path other
    /// than `/dev/null`).
    WriteRedirect,
}

/// Classify a shell command run with working directory `cwd` against the
/// project rooted at `project_root`.
pub fn classify(command: &str, cwd: &Path, project_root: &Path) -> Classified {
    let mut out = Classified {
        explorations: Vec::new(),
        read_only: !(command.contains("$(") || command.contains('`')),
        acked: false,
    };
    let (toks, heredoc) = tokenize(&strip_heredoc_bodies(command));
    if heredoc {
        out.read_only = false;
    }
    let mut cwd: Option<PathBuf> = Some(cwd.to_path_buf());
    let mut stage = 0usize;
    let mut words: Vec<String> = Vec::new();
    let flush =
        |words: &mut Vec<String>, stage: usize, cwd: &mut Option<PathBuf>, out: &mut Classified| {
            if !words.is_empty() {
                simple_command(words, stage, cwd, project_root, out);
                words.clear();
            }
        };
    for t in toks {
        match t {
            Tok::Word(w) => {
                if words.is_empty() || words.iter().all(|x| x.contains('=')) {
                    out.acked |= w.split_once('=').is_some_and(|(k, _)| k == ACK_VAR);
                }
                words.push(w);
            }
            Tok::Pipe => {
                flush(&mut words, stage, &mut cwd, &mut out);
                stage += 1;
            }
            Tok::Sep => {
                flush(&mut words, stage, &mut cwd, &mut out);
                stage = 0;
            }
            Tok::WriteRedirect => out.read_only = false,
        }
    }
    flush(&mut words, stage, &mut cwd, &mut out);
    out
}

/// Drop the bodies of `<<TAG` here-documents so code inside a Python or
/// shell heredoc is not read as commands.
fn strip_heredoc_bodies(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut lines = command.lines();
    while let Some(line) = lines.next() {
        out.push_str(line);
        out.push('\n');
        if let Some(tag) = heredoc_tag(line) {
            for body in lines.by_ref() {
                if body.trim() == tag {
                    break;
                }
            }
        }
    }
    out
}

fn heredoc_tag(line: &str) -> Option<String> {
    let idx = line.find("<<")?;
    let rest = line[idx + 2..].trim_start_matches(['-', '~']).trim_start();
    if rest.starts_with('<') {
        return None; // `<<<` here-string
    }
    let tag: String = rest
        .trim_start_matches(['\'', '"'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!tag.is_empty()).then_some(tag)
}

/// Split a command line into words and operators, honouring quotes. The
/// second value is true when a here-document or here-string was seen.
fn tokenize(s: &str) -> (Vec<Tok>, bool) {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut heredoc = false;
    let mut chars = s.chars().peekable();
    let push_word = |cur: &mut String, in_word: &mut bool, toks: &mut Vec<Tok>| {
        if *in_word {
            toks.push(Tok::Word(std::mem::take(cur)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                cur.push(n);
                            }
                        }
                        _ => cur.push(q),
                    }
                }
            }
            '\\' => {
                if let Some(n) = chars.next()
                    && n != '\n'
                {
                    in_word = true;
                    cur.push(n);
                }
            }
            ' ' | '\t' => push_word(&mut cur, &mut in_word, &mut toks),
            '\n' | ';' => {
                push_word(&mut cur, &mut in_word, &mut toks);
                toks.push(Tok::Sep);
            }
            '&' => {
                if chars.peek() == Some(&'>') {
                    // `&>file` / `&>>file`
                    chars.next();
                    if chars.peek() == Some(&'>') {
                        chars.next();
                    }
                    push_word(&mut cur, &mut in_word, &mut toks);
                    if redirect_target_writes(&mut chars) {
                        toks.push(Tok::WriteRedirect);
                    }
                    continue;
                }
                if in_word && cur.ends_with('>') {
                    // `2>&1`: fd duplication, part of a redirect word.
                    cur.push('&');
                    continue;
                }
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                push_word(&mut cur, &mut in_word, &mut toks);
                toks.push(Tok::Sep);
            }
            '|' => {
                push_word(&mut cur, &mut in_word, &mut toks);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    toks.push(Tok::Sep);
                } else {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                    }
                    toks.push(Tok::Pipe);
                }
            }
            '>' => {
                // A leading fd number (`2>`) is part of the redirect, not a word.
                let fd_prefix = in_word && cur.chars().all(|c| c.is_ascii_digit());
                if fd_prefix {
                    cur.clear();
                    in_word = false;
                } else {
                    push_word(&mut cur, &mut in_word, &mut toks);
                }
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2`, `2>&1`: duplication, writes no file.
                    chars.next();
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        chars.next();
                    }
                    continue;
                }
                if redirect_target_writes(&mut chars) {
                    toks.push(Tok::WriteRedirect);
                }
            }
            '<' => {
                push_word(&mut cur, &mut in_word, &mut toks);
                if chars.peek() == Some(&'<') {
                    heredoc = true;
                    chars.next();
                    while chars.peek().is_some_and(|c| matches!(c, '<' | '-' | '~')) {
                        chars.next();
                    }
                }
                // Skip the input file / heredoc tag word.
                while chars.peek() == Some(&' ') {
                    chars.next();
                }
                skip_word(&mut chars);
            }
            '(' | ')' | '{' | '}' if !in_word => {
                // Subshells and groups: treat as command boundaries.
                toks.push(Tok::Sep);
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    push_word(&mut cur, &mut in_word, &mut toks);
    (toks, heredoc)
}

fn skip_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let mut quote: Option<char> = None;
    while let Some(&c) = chars.peek() {
        match quote {
            Some(q) => {
                chars.next();
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')') {
                    break;
                }
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                chars.next();
            }
        }
    }
}

/// Consume a redirect target and report whether it writes a real file.
fn redirect_target_writes(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    while chars.peek() == Some(&' ') {
        chars.next();
    }
    let mut target = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')') {
            break;
        }
        if c != '\'' && c != '"' {
            target.push(c);
        }
        chars.next();
    }
    target != "/dev/null"
}

fn simple_command(
    words: &[String],
    stage: usize,
    cwd: &mut Option<PathBuf>,
    root: &Path,
    out: &mut Classified,
) {
    // Skip `VAR=value` prefixes and transparent wrappers.
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        let is_assign = w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if is_assign || matches!(w.as_str(), "env" | "time" | "command" | "nice" | "builtin") {
            i += 1;
        } else {
            break;
        }
    }
    let Some(prog_word) = words.get(i) else {
        return;
    };
    let prog = prog_word.rsplit('/').next().unwrap_or(prog_word).to_owned();
    let args: Vec<&str> = words[i + 1..].iter().map(String::as_str).collect();

    if !is_read_only(&prog, &args) {
        out.read_only = false;
    }
    if prog == "cd" {
        *cwd = match args.first() {
            Some(&d) if d != "-" => cwd.as_ref().map(|c| resolve(c, d)),
            _ => None,
        };
        return;
    }
    // A later pipeline stage only filters the previous stage's output.
    if stage > 0 {
        return;
    }
    let Some(cwd) = cwd.as_deref() else {
        return;
    };
    let found = match prog.as_str() {
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "ugrep" => {
            search(&prog, &args, cwd, root)
        }
        "cat" | "head" | "tail" | "nl" | "bat" | "less" | "more" => {
            read_files(&prog, &args, cwd, root)
        }
        "sed" => sed_read(&args, cwd, root),
        "awk" | "gawk" => awk_read(&args, cwd, root),
        "find" => find_code(&args, cwd, root),
        _ => Vec::new(),
    };
    out.explorations.extend(found);
}

fn is_read_only(prog: &str, args: &[&str]) -> bool {
    match prog {
        "git" => args
            .iter()
            .find(|a| !a.starts_with('-'))
            .is_some_and(|sub| READ_ONLY_GIT.contains(sub)),
        "sed" => !args.iter().any(|a| {
            *a == "--in-place"
                || a.starts_with("--in-place=")
                || (a.starts_with('-') && !a.starts_with("--") && a.contains('i'))
        }),
        "find" => !args.iter().any(|a| {
            matches!(
                *a,
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        _ => READ_ONLY_PROGRAMS.contains(&prog),
    }
}

/// Lexically resolve `p` against `cwd` (no filesystem access), expanding a
/// leading `~`.
fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let raw = if let Some(rest) = p.strip_prefix("~/") {
        std::env::var_os("HOME").map_or_else(|| PathBuf::from(p), |h| PathBuf::from(h).join(rest))
    } else if p == "~" {
        std::env::var_os("HOME").map_or_else(|| PathBuf::from(p), PathBuf::from)
    } else {
        cwd.join(p)
    };
    let mut out = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Project-relative path of `abs` if it is the project's own code area
/// (inside the root, not under a vendored/build directory).
fn project_rel(abs: &Path, root: &Path) -> Option<String> {
    // `$DIR/x.rs` was never expanded here, so where it points is unknown:
    // joined to the cwd it would look like a project file and count.
    if abs.to_string_lossy().contains('$') {
        return None;
    }
    let rel = abs.strip_prefix(root).ok()?;
    if rel
        .components()
        .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
    {
        return None;
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn is_code_path(p: &str) -> bool {
    Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| CODE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn glob_selects_code(glob: &str) -> bool {
    let g = glob.trim_start_matches('!');
    // `*.{rs,ts}` style alternations.
    if let (Some(open), Some(close)) = (g.find('{'), g.rfind('}')) {
        return g[open + 1..close]
            .split(',')
            .any(|ext| CODE_EXTENSIONS.contains(&ext.trim_start_matches('.')));
    }
    is_code_path(g)
}

/// Flags of `grep`/`rg` that consume the next word as their value.
fn search_flag_takes_value(prog: &str, flag: &str) -> bool {
    const GREP: &[&str] = &[
        "-e",
        "-f",
        "-m",
        "-A",
        "-B",
        "-C",
        "-d",
        "-D",
        "--regexp",
        "--file",
        "--max-count",
        "--after-context",
        "--before-context",
        "--context",
        "--include",
        "--exclude",
        "--exclude-dir",
        "--label",
    ];
    const RG: &[&str] = &[
        "-e",
        "-f",
        "-m",
        "-A",
        "-B",
        "-C",
        "-g",
        "-t",
        "-T",
        "-j",
        "-M",
        "-E",
        "--regexp",
        "--file",
        "--max-count",
        "--after-context",
        "--before-context",
        "--context",
        "--glob",
        "--iglob",
        "--type",
        "--type-not",
        "--type-add",
        "--threads",
        "--max-columns",
        "--max-depth",
        "--encoding",
        "--sort",
        "--sortr",
        "--colors",
        "--replace",
        "-r",
        "--max-filesize",
        "--path-separator",
        "--context-separator",
        "--field-match-separator",
    ];
    let list = if prog == "rg" { RG } else { GREP };
    list.contains(&flag)
}

fn search(prog: &str, args: &[&str], cwd: &Path, root: &Path) -> Vec<Exploration> {
    let mut pattern: Option<String> = None;
    let mut explicit_pattern = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut includes: Vec<String> = Vec::new();
    let mut types: Vec<String> = Vec::new();
    let mut recursive = prog != "grep" && prog != "egrep" && prog != "fgrep";
    let mut it = args.iter().copied().peekable();
    let mut end_of_flags = false;
    while let Some(a) = it.next() {
        if end_of_flags || !a.starts_with('-') || a == "-" {
            positionals.push(a);
            continue;
        }
        if a == "--" {
            end_of_flags = true;
            continue;
        }
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (a, None),
        };
        if matches!(
            flag,
            "-r" | "-R" | "--recursive" | "--dereference-recursive"
        ) && prog != "rg"
        {
            recursive = true;
            continue;
        }
        if !flag.starts_with("--") && flag.len() > 2 && prog != "rg" {
            // A short-flag cluster such as `-rn` or `-rnA3`.
            if flag[1..]
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .any(|c| c == 'r' || c == 'R')
            {
                recursive = true;
            }
            if flag.ends_with('e') {
                explicit_pattern = true;
                pattern = it.next().map(str::to_owned);
            }
            continue;
        }
        if search_flag_takes_value(prog, flag) {
            let value = inline.or_else(|| it.next().map(str::to_owned));
            match flag {
                "-e" | "--regexp" => {
                    explicit_pattern = true;
                    if pattern.is_none() {
                        pattern = value;
                    }
                }
                "-f" | "--file" => explicit_pattern = true,
                "--include" | "-g" | "--glob" | "--iglob" => includes.extend(value),
                "-t" | "--type" => types.extend(value),
                _ => {}
            }
        }
    }
    if !explicit_pattern && !positionals.is_empty() {
        pattern = Some(positionals.remove(0).to_owned());
    }
    // A pattern with no identifier in it (`"—\\|–"`, a Cyrillic range
    // regex) is a style or lint sweep, not navigation qartez can answer.
    if pattern.as_deref().is_some_and(|p| !has_identifier(p)) {
        return Vec::new();
    }
    let filtered = !includes.is_empty() || !types.is_empty();
    let filter_hits_code = includes.iter().any(|g| glob_selects_code(g))
        || types.iter().any(|t| CODE_RG_TYPES.contains(&t.as_str()));
    if filtered && !filter_hits_code {
        return Vec::new();
    }
    let hit = if positionals.is_empty() {
        recursive && project_rel(cwd, root).is_some() && dir_has_code(cwd)
    } else {
        positionals.iter().any(|p| {
            let abs = resolve(cwd, p);
            project_rel(&abs, root).is_some_and(|rel| {
                if is_code_path(&rel) {
                    true
                } else {
                    // A directory searched recursively counts only if it
                    // holds source code (a `manifests/` of YAML does not).
                    (recursive || prog == "rg") && abs.is_dir() && dir_has_code(&abs)
                }
            })
        })
    };
    if hit {
        vec![Exploration::Search { pattern }]
    } else {
        Vec::new()
    }
}

/// True when `pattern` holds a run of three or more letters, digits or
/// underscores once regex escapes (`\s`, `\x{0400}`) are removed.
fn has_identifier(pattern: &str) -> bool {
    let mut cleaned = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Drop the escape and, for `\x{...}`/`\p{...}`, its braces.
            if let Some(n) = chars.next()
                && matches!(n, 'x' | 'p' | 'P' | 'u')
                && chars.peek() == Some(&'{')
            {
                for b in chars.by_ref() {
                    if b == '}' {
                        break;
                    }
                }
            }
            cleaned.push(' ');
            continue;
        }
        cleaned.push(c);
    }
    let mut run = 0;
    for c in cleaned.chars() {
        if c.is_alphanumeric() || c == '_' {
            run += 1;
            if run >= 3 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Bounded breadth-first look for a source file under `dir`, skipping
/// vendored, build and hidden directories. Gives up (false) after a few
/// thousand entries so the hook stays fast on huge trees.
fn dir_has_code(dir: &Path) -> bool {
    const BUDGET: usize = 4000;
    let mut seen = 0usize;
    let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);
    while let Some(d) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > BUDGET {
                return false;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                    queue.push_back(entry.path());
                }
            } else if is_code_path(&name) {
                return true;
            }
        }
    }
    false
}

fn read_files(prog: &str, args: &[&str], cwd: &Path, root: &Path) -> Vec<Exploration> {
    let mut out = Vec::new();
    let mut end: Option<u32> = None;
    let mut it = args.iter().copied();
    while let Some(a) = it.next() {
        if a.starts_with('-') && a.len() > 1 {
            if matches!(prog, "head" | "tail") {
                let value = match a {
                    "-n" | "-c" => it.next().map(str::to_owned),
                    _ if a.starts_with("-n") => Some(a[2..].to_owned()),
                    _ => a[1..]
                        .chars()
                        .all(|c| c.is_ascii_digit())
                        .then(|| a[1..].to_owned()),
                };
                if prog == "head" && a != "-c" {
                    end = value.and_then(|v| v.parse().ok()).or(end);
                }
            }
            continue;
        }
        let abs = resolve(cwd, a);
        if let Some(rel) = project_rel(&abs, root)
            && is_code_path(&rel)
        {
            let start = (prog == "head" && end.is_some()).then_some(1);
            out.push(Exploration::Read {
                path: rel,
                start,
                end: if prog == "head" { end } else { None },
            });
        }
    }
    out
}

fn sed_read(args: &[&str], cwd: &Path, root: &Path) -> Vec<Exploration> {
    let quiet = args.iter().any(|a| {
        *a == "--quiet"
            || *a == "--silent"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('n'))
    });
    if !quiet {
        return Vec::new();
    }
    let mut script: Option<&str> = None;
    let mut files: Vec<&str> = Vec::new();
    let mut it = args.iter().copied();
    while let Some(a) = it.next() {
        if a == "-e" || a == "--expression" {
            script = it.next();
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            continue;
        }
        if script.is_none() {
            script = Some(a);
        } else {
            files.push(a);
        }
    }
    let (start, end) = script.map_or((None, None), sed_range);
    files
        .iter()
        .filter_map(|f| {
            let rel = project_rel(&resolve(cwd, f), root)?;
            is_code_path(&rel).then_some(Exploration::Read {
                path: rel,
                start,
                end,
            })
        })
        .collect()
}

/// Parse `120,180p` / `120p` into a line range.
fn sed_range(script: &str) -> (Option<u32>, Option<u32>) {
    let s = script.trim().trim_end_matches('p');
    match s.split_once(',') {
        Some((a, b)) => match (a.parse().ok(), b.parse().ok()) {
            (Some(a), Some(b)) => (Some(a), Some(b)),
            _ => (None, None),
        },
        None => match s.parse().ok() {
            Some(n) => (Some(n), Some(n)),
            None => (None, None),
        },
    }
}

fn awk_read(args: &[&str], cwd: &Path, root: &Path) -> Vec<Exploration> {
    let mut program_seen = args.contains(&"-f");
    let mut out = Vec::new();
    let mut it = args.iter().copied();
    while let Some(a) = it.next() {
        if matches!(a, "-F" | "-v" | "-f") {
            it.next();
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            continue;
        }
        if !program_seen {
            program_seen = true;
            continue;
        }
        if let Some(rel) = project_rel(&resolve(cwd, a), root)
            && is_code_path(&rel)
        {
            out.push(Exploration::Read {
                path: rel,
                start: None,
                end: None,
            });
        }
    }
    out
}

fn find_code(args: &[&str], cwd: &Path, root: &Path) -> Vec<Exploration> {
    let start = args
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .next()
        .copied()
        .unwrap_or(".");
    if project_rel(&resolve(cwd, start), root).is_none() {
        return Vec::new();
    }
    let names_code = args
        .windows(2)
        .any(|w| matches!(w[0], "-name" | "-iname") && glob_selects_code(w[1]));
    if names_code {
        vec![Exploration::Find]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for d in [
            "src/lib",
            "docs",
            "node_modules/pkg",
            "target/debug",
            "manifests/base",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in [
            "src/main.rs",
            "src/lib/api.ts",
            "docs/guide.md",
            "Cargo.toml",
            "manifests/base/app.yaml",
        ] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        Fixture { _dir: dir, root }
    }

    fn c(fx: &Fixture, cmd: &str) -> Classified {
        classify(cmd, &fx.root, &fx.root)
    }

    fn explores(fx: &Fixture, cmd: &str) -> bool {
        !c(fx, cmd).explorations.is_empty()
    }

    #[test]
    fn code_searches_count() {
        let fx = fixture();
        for cmd in [
            "grep -rn \"fn main\" src",
            "grep -n 'pub fn save' -A12 src/main.rs",
            "rg -n 'scrub' src",
            "rg scrub",
            "grep -rn \"kpk5rp\" src --include=*.svelte --include=*.ts | head -20",
            "rg -n foo -g '*.rs'",
            "rg -t rust foo",
            "grep -R foo .",
            "grep -rn \"Голос по кнопке\" src",
        ] {
            assert!(explores(&fx, cmd), "should count: {cmd}");
        }
    }

    #[test]
    fn code_reads_count() {
        let fx = fixture();
        assert_eq!(
            c(&fx, "sed -n 120,180p src/main.rs").explorations,
            vec![Exploration::Read {
                path: "src/main.rs".into(),
                start: Some(120),
                end: Some(180)
            }]
        );
        for cmd in [
            "cat src/main.rs",
            "cat -n src/lib/api.ts",
            "head -50 src/main.rs",
            "tail -n 20 src/main.rs",
            "awk '/fn/' src/main.rs",
            "cat src/main.rs | grep foo",
            "nl src/lib/api.ts",
            "find . -name '*.rs'",
        ] {
            assert!(explores(&fx, cmd), "should count: {cmd}");
        }
    }

    #[test]
    fn cd_into_the_project_is_followed() {
        let fx = fixture();
        let elsewhere = fx.root.parent().unwrap().to_path_buf();
        // Single-quoted so a Windows root (`\\?\C:\...`) keeps its
        // backslashes instead of losing them to shell escaping.
        let cmd = format!("cd '{}' && sed -n 1,40p src/main.rs", fx.root.display());
        assert!(!classify(&cmd, &elsewhere, &fx.root).explorations.is_empty());
        let cmd = format!("cd '{}'; grep -n foo src/main.rs", fx.root.display());
        assert!(!classify(&cmd, &elsewhere, &fx.root).explorations.is_empty());
        // `cd -` makes the directory unknown: nothing counts after it.
        assert!(!explores(&fx, "cd - && cat src/main.rs"));
    }

    #[test]
    fn non_code_and_filters_do_not_count() {
        let fx = fixture();
        for cmd in [
            "gh pr view 59 | grep -i codex",
            "sed -n 16,20p $S/replay.py",
            "cat \"$DIR/src/main.rs\"",
            "cd $W && cat src/main.rs",
            "git log --oneline -20 | rg render_stdout",
            "cargo test 2>&1 | rg -n panicked",
            "cargo test 2>&1 | grep FAIL",
            "git diff | grep foo",
            "grep foo",
            "grep -n '^\\[marketplaces' ~/.codex/config.toml",
            "grep -n '^#' docs/guide.md | tail -20",
            "rg -n foo docs/guide.md",
            "cat Cargo.toml",
            "sed -n 1,20p docs/guide.md",
            "grep -n 'fn upgrade' /tmp/scratch/upstream.rs",
            "rg -n foo node_modules/pkg",
            "grep -rn foo target/debug",
            "rg -n foo -g '*.md'",
            "grep -rn foo src --include=*.json",
            "sed 's/a/b/' src/main.rs",
            "sed -i '' 's/a/b/' src/main.rs",
            "ls src",
            "echo src/main.rs",
            "find . -name '*.md'",
            "grep -rn readinessProbe manifests",
            "grep -n \"—\\|–\" src/main.rs src/lib/api.ts",
            "grep -nP '^\\s*(//|/\\*).*[\\x{0400}-\\x{04FF}]' src/main.rs",
            "cat > src/new.rs <<'EOF'\nfn grep() {}\nEOF",
        ] {
            assert!(!explores(&fx, cmd), "should not count: {cmd}");
        }
    }

    #[test]
    fn heredoc_bodies_are_not_commands() {
        let fx = fixture();
        let cmd = "python3 - <<'EOF'\nimport os\ncat src/main.rs\nEOF";
        let got = c(&fx, cmd);
        assert!(got.explorations.is_empty());
        assert!(!got.read_only);
    }

    #[test]
    fn read_only_detection() {
        let fx = fixture();
        assert!(
            c(
                &fx,
                "sed -n 1,40p src/main.rs; grep -n foo src/main.rs | head"
            )
            .read_only
        );
        assert!(c(&fx, "cat src/main.rs 2>/dev/null").read_only);
        assert!(c(&fx, "rg foo src 2>&1 | head -5").read_only);
        assert!(c(&fx, "git log -3 && sed -n 1,9p src/main.rs").read_only);
        for cmd in [
            "sed -n 1,40p src/main.rs; cargo build",
            "cat src/main.rs > /tmp/copy.rs",
            "cat src/main.rs | tee out.txt",
            "grep -n foo src/main.rs && git commit -m x",
            "cat $(ls src/*.rs)",
            "sed -i 's/a/b/' src/main.rs",
            "find . -name '*.rs' -delete",
            "curl -s https://x | grep foo",
        ] {
            assert!(!c(&fx, cmd).read_only, "should be side-effecting: {cmd}");
        }
    }

    fn classified(read_only: bool) -> Classified {
        Classified {
            explorations: vec![Exploration::Search {
                pattern: Some("scrub".into()),
            }],
            read_only,
            acked: false,
        }
    }

    fn acked() -> Classified {
        Classified {
            acked: true,
            ..classified(true)
        }
    }

    const CFG: BashGuardConfig = BashGuardConfig {
        streak: 3,
        cooldown_secs: 120,
    };

    /// Drive a fresh state to its first denial at `now`.
    fn blocked_at(now: u64) -> StreakState {
        let mut st = StreakState::default();
        decide(&mut st, &classified(true), now, CFG);
        decide(&mut st, &classified(true), now, CFG);
        assert!(matches!(
            decide(&mut st, &classified(true), now, CFG),
            Verdict::Deny(_)
        ));
        st
    }

    #[test]
    fn streak_threshold_denies_with_advice() {
        let mut st = StreakState::default();
        let now = 10_000;
        assert_eq!(decide(&mut st, &classified(true), now, CFG), Verdict::Allow);
        assert_eq!(decide(&mut st, &classified(true), now, CFG), Verdict::Allow);
        let third = decide(&mut st, &classified(true), now, CFG);
        let Verdict::Deny(reason) = third else {
            panic!("third consecutive code search must be denied, got {third:?}")
        };
        assert!(reason.contains("qartez_find name=\"scrub\""), "{reason}");
        assert!(reason.contains("NOT run"), "{reason}");
        assert!(reason.contains("QARTEZ_SHELL=1"), "{reason}");
        assert!(!reason.contains("run it again"), "{reason}");
    }

    #[test]
    fn plain_retry_after_deny_is_denied_again() {
        let mut st = blocked_at(10_000);
        let v = decide(&mut st, &classified(true), 10_030, CFG);
        let Verdict::Deny(reason) = v else {
            panic!("a plain retry must stay blocked, got {v:?}")
        };
        assert!(reason.starts_with("Still blocked"), "{reason}");
    }

    #[test]
    fn qartez_call_ends_the_block() {
        let mut st = blocked_at(10_000);
        st.reset();
        assert_eq!(
            decide(&mut st, &classified(true), 10_010, CFG),
            Verdict::Allow
        );
        assert_eq!(st.blocks, 0);
    }

    #[test]
    fn ack_passes_and_starts_the_cooldown() {
        let mut st = blocked_at(10_000);
        assert_eq!(decide(&mut st, &acked(), 10_010, CFG), Verdict::Allow);
        assert_eq!(st.blocks, 0);
        // Inside the cooldown the streak may build but nothing is denied.
        for _ in 0..5 {
            assert_eq!(
                decide(&mut st, &classified(true), 10_060, CFG),
                Verdict::Allow
            );
        }
        // After it, the accumulated streak triggers again.
        assert!(matches!(
            decide(&mut st, &classified(true), 10_010 + 121, CFG),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn guard_gives_up_after_max_blocks() {
        let mut st = blocked_at(10_000);
        for _ in 1..MAX_BLOCKS {
            assert!(matches!(
                decide(&mut st, &classified(true), 10_010, CFG),
                Verdict::Deny(_)
            ));
        }
        assert_eq!(
            decide(&mut st, &classified(true), 10_020, CFG),
            Verdict::Allow
        );
        assert_eq!(st.blocks, 0);
        assert_eq!(st.last_nudge_unix, 10_020, "a give-up starts the cooldown");
    }

    #[test]
    fn block_expires() {
        let mut st = blocked_at(10_000);
        assert_eq!(
            decide(&mut st, &classified(true), 10_000 + BLOCK_EXPIRE_SECS, CFG),
            Verdict::Allow
        );
        assert_eq!(st.blocks, 0);
    }

    #[test]
    fn side_effecting_command_passes_during_a_block() {
        let mut st = blocked_at(10_000);
        assert_eq!(
            decide(&mut st, &classified(false), 10_010, CFG),
            Verdict::Allow
        );
        assert_eq!(st.blocks, 1, "the block stays for the next read-only read");
    }

    #[test]
    fn ack_prefix_is_detected_only_as_an_assignment() {
        let fx = fixture();
        assert!(c(&fx, "QARTEZ_SHELL=1 grep -n foo src/main.rs").acked);
        assert!(c(&fx, "A=1 QARTEZ_SHELL=1 sed -n 1,5p src/main.rs").acked);
        assert!(c(&fx, "cd src && QARTEZ_SHELL=1 cat main.rs").acked);
        assert!(
            !c(&fx, "QARTEZ_SHELL=1 grep -n foo src/main.rs")
                .explorations
                .is_empty()
        );
        assert!(!c(&fx, "grep -n foo src/main.rs").acked);
        assert!(!c(&fx, "grep -n QARTEZ_SHELL=1 src/main.rs").acked);
        assert!(!c(&fx, "QARTEZ_SHELLX=1 grep -n foo src/main.rs").acked);
    }

    #[test]
    fn side_effecting_commands_get_context_not_deny() {
        let cfg = BashGuardConfig::default();
        let mut st = StreakState {
            streak: cfg.streak - 1,
            ..StreakState::default()
        };
        let v = decide(&mut st, &classified(false), 1_000_000, cfg);
        assert!(matches!(v, Verdict::Context(_)), "{v:?}");
        assert_eq!(st.blocks, 0, "context must not start a block");
    }

    #[test]
    fn qartez_call_resets_and_non_code_keeps_streak() {
        let cfg = BashGuardConfig::default();
        let mut st = StreakState::default();
        decide(&mut st, &classified(true), 1_000_000, cfg);
        decide(&mut st, &Classified::default(), 1_000_000, cfg);
        assert_eq!(st.streak, 1);
        st.reset();
        assert_eq!(st.streak, 0);
    }

    #[test]
    fn home_directory_is_not_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let proj = home.join("code");
        std::fs::create_dir_all(&proj).unwrap();
        assert!(is_home_root(&home, Some(&home)));
        assert!(!is_home_root(&proj, Some(&home)));
        assert!(!is_home_root(&home, None));
    }

    #[test]
    fn zero_streak_disables() {
        let cfg = BashGuardConfig {
            streak: 0,
            cooldown_secs: 0,
        };
        let mut st = StreakState::default();
        for _ in 0..10 {
            assert_eq!(decide(&mut st, &classified(true), 5, cfg), Verdict::Allow);
        }
    }

    #[test]
    fn search_advice_fits_the_pattern() {
        let plain = advice(&[Exploration::Search {
            pattern: Some("render_stdout".into()),
        }]);
        assert!(
            plain.contains("qartez_find name=\"render_stdout\""),
            "{plain}"
        );
        let regex = advice(&[Exploration::Search {
            pattern: Some("regex escapes\\|its braces".into()),
        }]);
        assert!(!regex.contains("qartez_find"), "{regex}");
        assert!(
            regex.contains("qartez_grep query=\"regex\" search_bodies=true"),
            "{regex}"
        );
    }

    #[test]
    fn read_advice_carries_the_range() {
        let ex = vec![Exploration::Read {
            path: "src/main.rs".into(),
            start: Some(120),
            end: Some(180),
        }];
        let a = advice(&ex);
        assert!(
            a.contains("qartez_read file_path=\"src/main.rs\" start_line=120 limit=61"),
            "{a}"
        );
    }

    #[test]
    fn state_round_trips_and_keys_are_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let p = state_path(dir.path(), Some("../../etc/x y"), Some("agent 7"));
        assert!(p.starts_with(dir.path().join(".qartez").join("guard")));
        assert_eq!(p.file_name().unwrap(), "bash-etcxy-agent7.json");
        assert_eq!(load_state(&p), StreakState::default());
        let st = StreakState {
            streak: 2,
            last_nudge_unix: 42,
            blocks: 1,
            blocked_unix: 40,
        };
        save_state(&p, st);
        assert_eq!(load_state(&p), st);
        // A state file written before the block fields existed still loads.
        std::fs::write(&p, r#"{"streak":2,"last_nudge_unix":42}"#).unwrap();
        assert_eq!(
            load_state(&p),
            StreakState {
                streak: 2,
                last_nudge_unix: 42,
                ..StreakState::default()
            }
        );
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(load_state(&p), StreakState::default());
    }
}
