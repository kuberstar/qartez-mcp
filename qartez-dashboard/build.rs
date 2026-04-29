// Rust guideline compliant 2026-04-26
//! Build script that compiles the SvelteKit dashboard before the crate.
//!
//! Set `QARTEZ_SKIP_WEB_BUILD=1` to skip the frontend build (useful in CI
//! sandboxes that lack pnpm).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Per-bundle gzipped JS+CSS budget for `web/build/_app/immutable/`.
///
/// Excludes fonts (woff/woff2 are pre-compressed and stable) and
/// pre-compressed siblings (`.gz`/`.br`) so the number reflects what the
/// browser actually parses on first paint. Warning-only.
///
/// Raised from 100 KB in M9 to absorb the new settings page, reindex
/// button, EmptyState component, and WS reconnect plumbing.
const IMMUTABLE_GZIP_BUDGET_BYTES: u64 = 120 * 1024;

/// Extensions counted toward the budget. Anything else is excluded.
const BUDGETED_EXTENSIONS: &[&str] = &["js", "css"];

fn main() {
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/svelte.config.js");
    println!("cargo:rerun-if-env-changed=QARTEZ_SKIP_WEB_BUILD");

    if std::env::var_os("QARTEZ_SKIP_WEB_BUILD").is_some() {
        println!("cargo:warning=QARTEZ_SKIP_WEB_BUILD set; skipping `pnpm --dir web build`");
        return;
    }

    let pnpm = match find_pnpm() {
        Some(path) => path,
        None => {
            println!(
                "cargo:warning=pnpm not found on PATH; skipping web build. Set QARTEZ_SKIP_WEB_BUILD=1 to silence."
            );
            return;
        }
    };

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_dir = manifest_dir.join("web");

    if !web_dir.join("node_modules").exists() {
        run_or_fail(
            &pnpm,
            &["--dir", "web", "install", "--frozen-lockfile"],
            &manifest_dir,
        );
    }

    run_or_fail(&pnpm, &["--dir", "web", "build"], &manifest_dir);

    warn_on_oversized_bundle(&web_dir.join("build").join("_app").join("immutable"));
}

fn find_pnpm() -> Option<PathBuf> {
    let lookup = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(lookup).arg("pnpm").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let pick: &str = if cfg!(windows) {
        raw.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .find(|line| {
                let lower = line.to_ascii_lowercase();
                lower.ends_with(".cmd") || lower.ends_with(".exe") || lower.ends_with(".bat")
            })?
    } else {
        raw.lines().map(str::trim).find(|line| !line.is_empty())?
    };
    Some(PathBuf::from(pick))
}

fn run_or_fail(program: &Path, args: &[&str], cwd: &Path) {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|err| {
            panic!(
                "failed to spawn `{} {}`: {err}",
                program.display(),
                args.join(" ")
            )
        });
    if !status.success() {
        panic!(
            "`{} {}` exited with {status}",
            program.display(),
            args.join(" ")
        );
    }
}

fn warn_on_oversized_bundle(immutable_dir: &Path) {
    if !immutable_dir.exists() {
        return;
    }
    let mut total: u64 = 0;
    if let Err(err) = sum_gzipped_sizes(immutable_dir, &mut total) {
        println!("cargo:warning=could not measure bundle size: {err}");
        return;
    }
    if total > IMMUTABLE_GZIP_BUDGET_BYTES {
        println!(
            "cargo:warning=immutable bundle is {total} bytes gzipped, budget is {IMMUTABLE_GZIP_BUDGET_BYTES} bytes"
        );
    }
}

fn sum_gzipped_sizes(dir: &Path, total: &mut u64) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            sum_gzipped_sizes(&path, total)?;
        } else if file_type.is_file() && is_budgeted(&path) {
            let bytes = std::fs::read(&path)?;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&bytes)?;
            let compressed = encoder.finish()?;
            *total = total.saturating_add(compressed.len() as u64);
        }
    }
    Ok(())
}

fn is_budgeted(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| BUDGETED_EXTENSIONS.contains(&ext))
}
