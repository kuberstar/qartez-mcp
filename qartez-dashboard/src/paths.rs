//! Cross-platform paths used by the dashboard daemon.
//!
//! All daemon state lives under `~/.qartez/`:
//!
//! - `dashboard.pid`  - PID of the running daemon, used by `qartez dashboard stop`
//! - `dashboard.port` - port the HTTP server is bound to (regenerated on each start)
//! - `auth.token`     - 32 random bytes hex-encoded, mode 0600 (regenerated on each start)

use std::path::PathBuf;

use anyhow::{Context, Result};

const QARTEZ_DIR_NAME: &str = ".qartez";
const PID_FILE: &str = "dashboard.pid";
const PORT_FILE: &str = "dashboard.port";
const TOKEN_FILE: &str = "auth.token";

/// `~/.qartez/`, creating it if missing.
pub fn qartez_dir() -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .context("could not resolve user home directory")?
        .home_dir()
        .to_path_buf();
    let dir = home.join(QARTEZ_DIR_NAME);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    Ok(dir)
}

pub fn pid_file() -> Result<PathBuf> {
    Ok(qartez_dir()?.join(PID_FILE))
}

pub fn port_file() -> Result<PathBuf> {
    Ok(qartez_dir()?.join(PORT_FILE))
}

pub fn token_file() -> Result<PathBuf> {
    Ok(qartez_dir()?.join(TOKEN_FILE))
}
