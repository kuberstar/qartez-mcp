//! Daemon process management: PID file, port file, auth token,
//! start / stop / status / open subcommand handlers.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio_util::sync::CancellationToken;

use crate::auth;
use crate::indexer;
use crate::paths;
use crate::server;
use crate::state::AppState;
use crate::watcher;

const STATUS_LIVENESS_TIMEOUT_MS: u64 = 500;

/// Default TCP port. Picked once and pinned so the dashboard URL is stable
/// across restarts and bookmark-friendly. Falls back to an ephemeral port
/// if 7777 is already in use.
pub const DEFAULT_PORT: u16 = 7777;

/// Start the daemon. `foreground=true` blocks until shutdown; otherwise
/// the caller is responsible for backgrounding (launchd / nohup / shell &).
pub async fn start(
    project_root: Option<PathBuf>,
    foreground: bool,
    port: Option<u16>,
    indexer: indexer::IncrementalIndexer,
) -> Result<()> {
    refuse_root()?;
    if let Some(pid) = read_pid_if_alive()? {
        bail!("dashboard already running (pid {pid})");
    }

    let project_root = match project_root {
        Some(p) => p
            .canonicalize()
            .with_context(|| format!("canonicalizing {}", p.display()))?,
        None => std::env::current_dir().context("reading current directory")?,
    };

    let token = auth::generate_token();
    auth::write_token(&paths::token_file()?, &token)?;

    let listener = server::bind(Some(port.unwrap_or(DEFAULT_PORT))).await?;
    let port = listener.local_addr()?.port();
    write_port(port)?;
    write_pid(std::process::id())?;

    let shutdown = CancellationToken::new();
    let state = AppState::new(project_root.clone(), token, shutdown.clone());

    watcher::spawn(state.clone())?;
    indexer::spawn(state.clone(), indexer);

    install_shutdown_hooks(shutdown.clone());

    tracing::info!(
        port,
        project_root = %project_root.display(),
        foreground,
        "dashboard.start"
    );

    let result = server::serve(listener, state, shutdown).await;

    cleanup_pid_and_port();
    result
}

pub async fn stop() -> Result<()> {
    let pid_path = paths::pid_file()?;
    let pid_str = std::fs::read_to_string(&pid_path)
        .with_context(|| format!("reading {}", pid_path.display()))?;
    let pid: i32 = pid_str
        .trim()
        .parse()
        .with_context(|| format!("parsing pid from {}", pid_path.display()))?;
    send_sigterm(pid)?;
    let _ = std::fs::remove_file(&pid_path);
    let _ = std::fs::remove_file(paths::port_file()?);
    println!("sent SIGTERM to pid {pid}");
    Ok(())
}

pub async fn status() -> Result<()> {
    let port = read_port().ok();
    let pid = read_pid_if_alive()?;
    match (pid, port) {
        (Some(pid), Some(port)) => {
            let alive = ping(port).await;
            println!(
                "dashboard: {} (pid {pid}, port {port})",
                if alive {
                    "running"
                } else {
                    "stale (no http response)"
                }
            );
        }
        (Some(pid), None) => {
            println!("dashboard: pid file present (pid {pid}) but port file missing")
        }
        (None, _) => println!("dashboard: not running"),
    }
    Ok(())
}

pub async fn open() -> Result<()> {
    let port = read_port().context("dashboard not running (no port file)")?;
    let token = auth::read_token().context("auth token unreadable")?;
    let url = format!("http://qartez.localhost:{port}/auth?token={token}");
    println!("opening {url}");
    open_in_browser(&url)?;
    Ok(())
}

#[cfg(unix)]
fn refuse_root() -> Result<()> {
    use nix::unistd::Uid;
    if Uid::effective().is_root() {
        bail!(
            "refusing to start as root - the daemon writes to ~/.qartez and the browser session would not match"
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_root() -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn send_sigterm(pid: i32) -> Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid), Signal::SIGTERM).map_err(|e| anyhow!("kill: {e}"))?;
    Ok(())
}

#[cfg(not(unix))]
fn send_sigterm(_pid: i32) -> Result<()> {
    bail!("`qartez dashboard stop` is unix-only in MVP")
}

#[cfg(unix)]
fn install_shutdown_hooks(token: CancellationToken) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(error) => {
                tracing::error!(?error, "signal.sigterm.install_failed");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        tracing::info!("dashboard.shutdown.signal");
        token.cancel();
    });
}

#[cfg(not(unix))]
fn install_shutdown_hooks(token: CancellationToken) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        token.cancel();
    });
}

fn write_port(port: u16) -> Result<()> {
    let path = paths::port_file()?;
    atomic_write(&path, port.to_string().as_bytes())
}

fn write_pid(pid: u32) -> Result<()> {
    let path = paths::pid_file()?;
    atomic_write(&path, pid.to_string().as_bytes())
}

/// Tempfile + rename: avoids torn writes if a second process reads the
/// port mid-write.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn read_port() -> Result<u16> {
    let path = paths::port_file()?;
    let s =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    s.trim()
        .parse()
        .with_context(|| format!("parsing port from {}", path.display()))
}

fn read_pid_if_alive() -> Result<Option<i32>> {
    let path = paths::pid_file()?;
    let Ok(s) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(pid) = s.trim().parse::<i32>() else {
        return Ok(None);
    };
    if pid_alive(pid) {
        Ok(Some(pid))
    } else {
        Ok(None)
    }
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid), None).is_ok()
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

fn cleanup_pid_and_port() {
    if let Ok(p) = paths::pid_file() {
        let _ = std::fs::remove_file(p);
    }
    if let Ok(p) = paths::port_file() {
        let _ = std::fs::remove_file(p);
    }
}

async fn ping(port: u16) -> bool {
    let connect = tokio::net::TcpStream::connect(("127.0.0.1", port));
    matches!(
        tokio::time::timeout(Duration::from_millis(STATUS_LIVENESS_TIMEOUT_MS), connect).await,
        Ok(Ok(_))
    )
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> Result<()> {
    std::process::Command::new("open")
        .arg(url)
        .status()
        .context("spawning `open`")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_in_browser(url: &str) -> Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .status()
        .context("spawning `xdg-open`")?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> Result<()> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .context("spawning `start`")?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_in_browser(_url: &str) -> Result<()> {
    bail!("`qartez dashboard open` is not supported on this platform")
}
