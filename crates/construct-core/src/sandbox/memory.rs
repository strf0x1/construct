//! The memory ceiling on a tool's process tree (#230, ADR 0061): the helper's
//! watchdog and the line it leaves when it fires.
//!
//! The design choice: the supervising helper samples the real memory of the
//! whole tree it already owns and kills the tree past `[sandbox] memory_max`,
//! instead of a cgroup (needs systemd delegation, absent in CI, containers and
//! on macOS) or an rlimit (per process, and `RLIMIT_AS` counts address space,
//! not memory). Because the helper did the kill, the cause is definite and the
//! model is told so in prose. What a sample counts is per platform, in
//! `supervise.rs` (anonymous resident plus swapped) and `supervise_macos.rs`
//! (physical footprint, which includes compressed pages).

use std::time::Duration;

/// Prefix of the helper's stderr line when it killed the tree for memory:
/// `construct-memory <used bytes> <limit bytes> <prose>`.
pub const MEMORY: &str = "construct-memory";

/// How often the helper samples. A runaway grows about 1.5 GB/s (#230), so
/// the tree can pass the limit by a few hundred MB before it is killed.
pub const SAMPLE: Duration = Duration::from_millis(100);

/// The line the helper writes when it kills the tree. The trailing prose is
/// for readers that show the helper's stderr raw (an MCP server's error).
pub fn line(used: u64, limit: u64) -> String {
    format!(
        "{MEMORY} {used} {limit} killed: the process tree used {} of memory, over the {} limit ([sandbox] memory_max)",
        size(used),
        size(limit)
    )
}

/// `(used, limit)` from one of the helper's lines, if it is the memory line.
pub fn parse(line: &str) -> Option<(u64, u64)> {
    let mut f = line.strip_prefix(MEMORY)?.split_whitespace();
    Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
}

/// What the model reads after its command was killed for memory.
pub fn note(used: u64, limit: u64) -> String {
    format!(
        "[killed: the command used {} of memory, over the sandbox's {} limit ([sandbox] memory_max). \
         It (or something it started) kept allocating; do not rerun it as is. Bound the input, \
         stream instead of collecting, or check for an unbounded loop or infinite iterator.]",
        size(used),
        size(limit)
    )
}

/// Bytes as the nearest binary unit, one decimal.
pub fn size(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{b} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_round_trips_and_reads_as_prose() {
        let l = line(3 << 30, 1 << 30);
        assert_eq!(parse(&l), Some((3 << 30, 1 << 30)));
        assert!(l.contains("3.0 GiB") && l.contains("1.0 GiB limit"), "{l}");
        assert_eq!(parse("construct-swept 12 sleep"), None);
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536 << 20), "1.5 GiB");
    }
}
