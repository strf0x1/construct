//! `[sandbox]`: the tool boundary's settings, and `memory_max` read as bytes.
//!
//! `memory_max` is a share of this machine's RAM by default, never a fixed
//! number: one fixed size is wrong on either an 8 GB laptop or a 128 GB
//! workstation (#230, ADR 0061). The sandbox helper enforces it.

use serde::{Deserialize, Serialize};

/// What `memory_max` is when the config doesn't say.
pub const MEMORY_MAX_DEFAULT: &str = "50%";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// "workspace" (default): tools read and write the working directory and
    /// read system paths and toolchains, nothing else under $HOME.
    /// "read-only": no writes outside construct's temp dir. "off": no boundary.
    pub mode: String,
    /// Extra paths tools may read (`~/` expands).
    pub allow_read: Vec<String>,
    /// Extra paths tools may read and write.
    pub allow_write: Vec<String>,
    /// The most memory one shell call's process tree (or one MCP server's)
    /// may hold before it is killed: a share of RAM (`"50%"`), a size
    /// (`"8G"`, `"512M"`, bytes), or `"off"`. Applies with any `mode`.
    pub memory_max: String,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: "workspace".into(),
            allow_read: vec![],
            allow_write: vec![],
            memory_max: MEMORY_MAX_DEFAULT.into(),
        }
    }
}

impl SandboxConfig {
    /// `memory_max` in bytes, `None` when off (or when RAM can't be read for
    /// a share), or why the setting doesn't parse.
    pub fn memory_max_bytes(&self) -> Result<Option<u64>, String> {
        parse_memory(&self.memory_max, physical_memory())
    }
}

/// `setting` in bytes against `ram` bytes of RAM.
fn parse_memory(setting: &str, ram: Option<u64>) -> Result<Option<u64>, String> {
    let s = setting.trim().to_ascii_lowercase();
    let bad = || {
        format!(
            "[sandbox] memory_max = {setting:?}: expected a share of RAM (\"50%\"), a size (\"8G\", \"512M\") or \"off\""
        )
    };
    if s == "off" || s.is_empty() {
        return Ok(None);
    }
    if let Some(pct) = s.strip_suffix('%') {
        let pct: f64 = pct.trim().parse().map_err(|_| bad())?;
        if !pct.is_finite() || pct <= 0.0 || pct > 100.0 {
            return Err(bad());
        }
        return Ok(ram.map(|r| (r as f64 * pct / 100.0) as u64));
    }
    let s = s.strip_suffix('b').unwrap_or(&s);
    let s = s.strip_suffix('i').unwrap_or(s);
    let (num, shift) = match s.char_indices().last() {
        Some((i, 'k')) => (s.get(..i), 10),
        Some((i, 'm')) => (s.get(..i), 20),
        Some((i, 'g')) => (s.get(..i), 30),
        Some((i, 't')) => (s.get(..i), 40),
        _ => (Some(s), 0),
    };
    let n: f64 = num.unwrap_or_default().trim().parse().map_err(|_| bad())?;
    if !n.is_finite() || n <= 0.0 {
        return Err(bad());
    }
    Ok(Some((n * (1u64 << shift) as f64) as u64))
}

/// Installed RAM in bytes.
fn physical_memory() -> Option<u64> {
    // SAFETY: sysconf only reads system constants.
    let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
    (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_max_reads_shares_sizes_and_off() {
        let ram = Some(16 << 30);
        assert_eq!(parse_memory("50%", ram), Ok(Some(8 << 30)));
        assert_eq!(parse_memory("8G", ram), Ok(Some(8 << 30)));
        assert_eq!(parse_memory("512MiB", ram), Ok(Some(512 << 20)));
        assert_eq!(parse_memory("1.5g", ram), Ok(Some(3 << 29)));
        assert_eq!(parse_memory("4096", ram), Ok(Some(4096)));
        assert_eq!(parse_memory("off", ram), Ok(None));
        assert_eq!(parse_memory("50%", None), Ok(None));
        for bad in ["lots", "0%", "150%", "-1G", "0", "infG", "nan%"] {
            assert!(parse_memory(bad, ram).is_err(), "{bad}");
        }
    }

    #[test]
    fn this_machine_has_ram() {
        assert!(physical_memory().is_some_and(|b| b > 1 << 20));
    }
}
