//! The knobs for background jobs: how many may run at once, and how long a
//! background shell command may live.

use serde::{Deserialize, Serialize};

/// `[jobs]`: limits that cover every kind of job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JobsConfig {
    /// Jobs running at once, background delegates and commands together.
    pub limit: usize,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self { limit: 4 }
    }
}

/// `[tools.shell]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// Seconds a background command may run before it is stopped and ends as
    /// `timeout`. A call's own `timeout` can only shorten it.
    pub background_timeout: u64,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self { background_timeout: 30 * 60 }
    }
}
