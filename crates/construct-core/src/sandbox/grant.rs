//! What a delegated child asks of the sandbox: paths and a network switch.
//! `Policy::for_agent` checks it against the parent's policy, so the sandbox
//! never reads an agent definition; the delegate tool builds one from it.

use std::path::PathBuf;

/// Paths a child may read and read-write (`~/` already expanded), and
/// whether it may reach the network.
pub struct Grant {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub network: bool,
}

impl Grant {
    /// Paths as a definition lists them, with `~/` expanded.
    pub fn new(read: &[String], write: &[String], network: bool) -> Self {
        let expanded = |paths: &[String]| paths.iter().map(|p| super::expand_home(p)).collect();
        Self { read: expanded(read), write: expanded(write), network }
    }
}
