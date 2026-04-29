//! Project auto-discovery.
//!
//! M1 scope: stub. Real implementation lands in M4 - scans `~/code`,
//! `~/Documents/GitHub`, `~/projects`, `~/dev`, `~/src` for `.git` directories,
//! sorts by mtime, queues background indexing.
