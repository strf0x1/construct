//! Declarative `[[wakes]]` and `[wake]` configuration (ADR 0079, #416).
//!
//! The design choice: wake config lives here as plain serde data and pure
//! validation so `config` never depends on `wake` (no `config <-> wake` cycle)
//! and stamped peer binaries carry the exact same `[[wakes]]` schema in their
//! `[config]` layer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Duration;

/// One declarative `[[wakes]]` trigger entry in `config.toml` or a peer stamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WakeTriggerConfig {
    /// Unique trigger name (used by `construct wake emit <name>`, dedicated
    /// lane lookup, and `Head.origin`).
    pub name: String,
    /// Producer kind: `"timer"`, `"command"`, `"cli"`, or an extension kind.
    pub kind: String,
    /// Schedule interval for `timer` and `command` triggers (`"500ms"`, `"30s"`, `"5m"`, `"1h"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// Shell command executed under the sandbox for `kind = "command"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Session routing policy: `"fresh"`, `"dedicated"`, `"dedicated:<lane>"`, or `"shared"`.
    pub session: String,
    /// Minimum quiet period between delivered wake batches for this trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<String>,
    /// Debounce/coalescing window to batch rapid bursts into one wake turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce: Option<String>,
    /// Expiry TTL for queued/unconsumed wakes on restart; defaults to `[wake].default_ttl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// Operator-authored instruction delivered alongside the wake payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Optional substring filter checked against `summary` before waking the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_summary: Option<String>,
    /// Explicit trust override (`true` = untrusted data fenced from instructions).
    /// When omitted: `timer` defaults to trusted (`false`); `command` and `cli`
    /// default to untrusted (`true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untrusted: Option<bool>,
    /// Whether wake turns for this trigger may spawn background learning (`learn/`).
    /// Defaults to `false` so routine headless checks do not churn the learner.
    #[serde(default)]
    pub learn: bool,
}

impl Default for WakeTriggerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: String::new(),
            every: None,
            command: None,
            session: "dedicated".into(),
            cooldown: None,
            coalesce: None,
            ttl: None,
            prompt: None,
            match_summary: None,
            untrusted: None,
            learn: false,
        }
    }
}

impl WakeTriggerConfig {
    /// Effective untrusted setting for this trigger.
    pub fn is_untrusted(&self) -> bool {
        self.untrusted.unwrap_or_else(|| self.kind != "timer")
    }

    pub fn every_duration(&self) -> Result<Option<Duration>, String> {
        parse_opt_duration(self.every.as_deref(), "every")
    }

    pub fn cooldown_duration(&self) -> Result<Duration, String> {
        Ok(parse_opt_duration(self.cooldown.as_deref(), "cooldown")?.unwrap_or(Duration::ZERO))
    }

    pub fn coalesce_duration(&self) -> Result<Duration, String> {
        Ok(parse_opt_duration(self.coalesce.as_deref(), "coalesce")?.unwrap_or(Duration::ZERO))
    }

    pub fn ttl_duration(&self, default: Duration) -> Result<Duration, String> {
        Ok(parse_opt_duration(self.ttl.as_deref(), "ttl")?.unwrap_or(default))
    }
}

/// Process-wide `[wake]` runtime limits for `SessionManager` and `WakeBus`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WakeRuntimeConfig {
    /// Maximum concurrent wake turns active in one process.
    pub max_concurrent_turns: usize,
    /// Drop idle wake `Agent` instances from memory after this duration (rehydrated on next wake).
    pub idle_evict: String,
    /// Default expiry TTL for unconsumed wakes on restart so reboots do not stampede stale ticks.
    pub default_ttl: String,
}

impl Default for WakeRuntimeConfig {
    fn default() -> Self {
        Self { max_concurrent_turns: 2, idle_evict: "5m".into(), default_ttl: "15m".into() }
    }
}

impl WakeRuntimeConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn idle_evict_duration(&self) -> Result<Duration, String> {
        parse_duration(&self.idle_evict).map_err(|e| format!("wake.idle_evict: {e}"))
    }

    pub fn default_ttl_duration(&self) -> Result<Duration, String> {
        parse_duration(&self.default_ttl).map_err(|e| format!("wake.default_ttl: {e}"))
    }
}

/// Parse a compact human duration (`"250ms"`, `"10s"`, `"5m"`, `"1h"`, `"2d"`).
pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("duration cannot be empty".into());
    }
    let (digits, mult_ms): (&str, u64) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400_000)
    } else {
        return Err(format!("invalid duration {raw:?} (expected suffix ms, s, m, h, or d)"));
    };
    let n: u64 = digits.trim().parse().map_err(|_| format!("invalid duration {raw:?}"))?;
    let ms = n.checked_mul(mult_ms).ok_or_else(|| format!("duration overflow: {raw:?}"))?;
    Ok(Duration::from_millis(ms))
}

fn parse_opt_duration(raw: Option<&str>, field: &str) -> Result<Option<Duration>, String> {
    match raw {
        None => Ok(None),
        Some(s) => parse_duration(s).map(Some).map_err(|e| format!("{field}: {e}")),
    }
}

/// Validate `[wake]` and all `[[wakes]]` entries on config load.
pub fn validate(runtime: &WakeRuntimeConfig, triggers: &[WakeTriggerConfig]) -> Result<(), String> {
    if runtime.max_concurrent_turns == 0 {
        return Err("wake.max_concurrent_turns must be at least 1".into());
    }
    runtime.idle_evict_duration()?;
    let default_ttl = runtime.default_ttl_duration()?;
    let mut names = BTreeSet::new();
    for (idx, t) in triggers.iter().enumerate() {
        let name = t.name.trim();
        if name.is_empty() {
            return Err(format!("wakes[{idx}].name cannot be empty"));
        }
        if !names.insert(name) {
            return Err(format!("duplicate wake trigger name {name:?}"));
        }
        let kind = t.kind.trim();
        if kind.is_empty() {
            return Err(format!("wakes[{name}].kind cannot be empty"));
        }
        validate_session(&t.session).map_err(|e| format!("wakes[{name}].session: {e}"))?;
        let every = t.every_duration().map_err(|e| format!("wakes[{name}].{e}"))?;
        t.cooldown_duration().map_err(|e| format!("wakes[{name}].{e}"))?;
        t.coalesce_duration().map_err(|e| format!("wakes[{name}].{e}"))?;
        t.ttl_duration(default_ttl).map_err(|e| format!("wakes[{name}].{e}"))?;
        if kind == "timer" {
            match every {
                Some(d) if !d.is_zero() => {}
                _ => return Err(format!("wakes[{name}]: timer requires a non-zero `every` duration")),
            }
        }
        if kind == "command" {
            if t.command.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err(format!("wakes[{name}]: command trigger requires `command`"));
            }
            match every {
                Some(d) if !d.is_zero() => {}
                _ => return Err(format!("wakes[{name}]: command trigger requires a non-zero `every` duration")),
            }
        }
    }
    Ok(())
}

fn validate_session(raw: &str) -> Result<(), String> {
    let s = raw.trim();
    if matches!(s, "fresh" | "dedicated" | "shared") {
        return Ok(());
    }
    if let Some(lane) = s.strip_prefix("dedicated:")
        && !lane.trim().is_empty()
    {
        return Ok(());
    }
    Err(format!("expected \"fresh\", \"dedicated\", \"dedicated:<lane>\", or \"shared\", got {raw:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_durations_and_rejects_bad_ones() {
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("5w").is_err());
    }

    #[test]
    fn validates_triggers_and_catches_duplicates_and_missing_fields() {
        let rt = WakeRuntimeConfig::default();
        let ok = WakeTriggerConfig {
            name: "health".into(),
            kind: "timer".into(),
            every: Some("30m".into()),
            session: "dedicated".into(),
            cooldown: Some("5m".into()),
            ..Default::default()
        };
        assert!(validate(&rt, std::slice::from_ref(&ok)).is_ok());
        assert!(validate(&rt, &[ok.clone(), ok]).is_err());
        let bad_timer = WakeTriggerConfig { name: "t".into(), kind: "timer".into(), ..Default::default() };
        assert!(validate(&rt, &[bad_timer]).is_err());
        let bad_cmd = WakeTriggerConfig {
            name: "c".into(),
            kind: "command".into(),
            every: Some("10s".into()),
            ..Default::default()
        };
        assert!(validate(&rt, &[bad_cmd]).is_err());
        let bad_session =
            WakeTriggerConfig { name: "cli".into(), kind: "cli".into(), session: "bogus".into(), ..Default::default() };
        assert!(validate(&rt, &[bad_session]).is_err());
    }
}
