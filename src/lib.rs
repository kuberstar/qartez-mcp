pub mod cli;
pub mod cli_runner;
pub mod config;
pub mod error;
pub mod git;
pub mod graph;
pub mod guard;
pub mod index;
pub mod server;
pub mod storage;
pub(crate) mod str_utils;
pub mod toolchain;
pub mod watch;

#[cfg(feature = "benchmark")]
pub mod benchmark;

#[cfg(feature = "semantic")]
pub mod embeddings;
