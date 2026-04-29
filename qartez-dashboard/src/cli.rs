//! Clap subcommand definitions for `qartez dashboard ...`.

use clap::Subcommand;

#[derive(Debug, Clone, Subcommand)]
pub enum DashboardCommand {
    /// Start the dashboard daemon (HTTP + WebSocket server on 127.0.0.1).
    Start {
        /// Run in the foreground instead of detaching. Useful for development
        /// and for letting launchd/systemd manage the process directly.
        #[arg(long)]
        foreground: bool,

        /// TCP port to bind on 127.0.0.1. Defaults to 7777. Falls back to an
        /// OS-assigned ephemeral port if the requested port is already in use.
        #[arg(long)]
        port: Option<u16>,
    },

    /// Stop the running dashboard daemon (SIGTERM via PID file).
    Stop,

    /// Print daemon status (running / port / PID / index counts).
    Status,

    /// Open the dashboard in the default browser, performing the auth handshake.
    Open,
}
