pub mod cli;
pub mod config;
pub mod error;
pub mod git;
pub mod graph;
pub mod guard;
pub mod index;
pub mod server;
pub mod storage;
pub mod toolchain;
pub mod watch;

#[cfg(feature = "benchmark")]
pub mod benchmark;

#[cfg(feature = "semantic")]
pub mod embeddings;
