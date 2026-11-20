//! Wall-clock readings as plain integers. A leaf on purpose: the journal,
//! the learners, the skill library and several tools only need the time, and
//! none of them should depend on one another to get it. A clock set before
//! the epoch reads as 0 rather than failing.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Nanoseconds since the Unix epoch, for names that must not collide.
pub fn now_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}
