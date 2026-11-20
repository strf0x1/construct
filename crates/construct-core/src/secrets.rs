//! Secrets through secretspec. Construct declares its spec in Rust, so there is
//! no secretspec.toml to ship. On a host values come from the environment first,
//! then the OS keyring, and `construct secrets set NAME` writes to the keyring.
//! In a Deck-managed guest (`DECK_MANAGED`) the host owns every credential: the
//! environment holds placeholders the host gateway replaces, the keyring is
//! never consulted or written, and status says which values are placeholders.
//! An optional feature (web search, the Jev router, an MCP secret) resolves
//! nothing there even if the guest environment holds a value (ADR 0039).
//!
//! Without the `keyring` feature (ADR 0051) the secretspec subtree is not
//! compiled in at all: every value comes from the environment, `set` refuses,
//! and the facade below is unchanged — a preconfigured drop build (#271)
//! resolves its one endpoint key from the environment or a bootstrap flow.

use anyhow::{Result, anyhow};
#[cfg(feature = "keyring")]
use secretspec::{NamedResolution, Secret, Secrets, Spec};
#[cfg(feature = "keyring")]
use std::collections::HashMap;
#[cfg(feature = "keyring")]
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const EXA_API_KEY: &str = "EXA_API_KEY";
pub const BRAVE_API_KEY: &str = "BRAVE_API_KEY";
pub const TAVILY_API_KEY: &str = "TAVILY_API_KEY";

/// Deck sets this in a VM task's guest environment (deck ADR 0004 §4). Every
/// environment secret in the guest is a placeholder the host gateway replaces.
pub const DECK_MANAGED: &str = "DECK_MANAGED";

/// The one line `construct secrets set` refuses with in a Deck guest: there is
/// no guest credential store, and pretending otherwise would mislead the user.
pub const HOST_OWNED: &str = "secrets are host-owned in this Deck task and injected by Deck at the gateway; use Deck's host-side credential UI or command";

/// The one line `construct secrets set` refuses with in an env-only build:
/// there is no credential store compiled in, and the environment is the source.
pub const ENV_ONLY: &str = "this build has no keyring (env-only secrets); export the value in the environment instead";

pub const KNOWN: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "Key for the OpenAI-compatible model endpoint"),
    ("OPENROUTER_API_KEY", "OpenRouter key, if you keep it under that name"),
    ("TYPESAFE_API_KEY", "TypeSafe key for the Jev router"),
    (EXA_API_KEY, "Exa key for web_search (optional)"),
    (BRAVE_API_KEY, "Brave Search key for web_search (optional)"),
    (TAVILY_API_KEY, "Tavily key for web_search (optional)"),
];

#[cfg(feature = "keyring")]
fn spec(extra: Option<&str>) -> Result<Secrets> {
    let mut b = Spec::builder("construct").default_providers(["env", "keyring"]);
    for (name, desc) in KNOWN {
        b = b.secret(*name, Secret::optional(*desc));
    }
    if let Some(name) = extra.filter(|n| !KNOWN.iter().any(|(k, _)| k == n)) {
        b = b.secret(name, Secret::optional("Declared in config.toml"));
    }
    let spec = b.build().map_err(|e| anyhow!("secretspec: {e}"))?;
    Secrets::from_spec(spec).map_err(|e| anyhow!("secretspec: {e}"))
}

#[cfg(feature = "keyring")]
static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

#[cfg(feature = "keyring")]
#[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
fn cache() -> MutexGuard<'static, HashMap<String, Option<String>>> {
    CACHE.get_or_init(Default::default).lock().unwrap()
}

/// Whether this process is a guest in a Deck-managed task. The marker must be
/// set and not a falsey word, so a stray `DECK_MANAGED=0` cannot turn it on.
pub fn managed() -> bool {
    managed_value(std::env::var(DECK_MANAGED).ok().as_deref())
}

fn managed_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    })
}

/// A non-empty environment value: the environment is the common case and costs
/// nothing, while the keyring may wake a D-Bus service.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Resolve one secret, once per process (misses are cached too). The
/// environment is checked first. A Deck guest stops there: the host owns the
/// value, so there is no keyring to consult.
pub fn get(name: &str) -> Option<String> {
    if let Some(value) = env_value(name) {
        return Some(value);
    }
    if managed() {
        return None;
    }
    keyring(name)
}

/// Resolve a secret an optional feature asked for: a web search backend, the
/// Jev router, or an MCP server's declared secret. A Deck guest resolves
/// none: the host owns every credential, and a key that happens to be in the
/// guest environment is not a grant (Deck ADR 0004 §10). The endpoint key is
/// not optional and goes through `get`.
pub fn optional(name: &str) -> Option<String> {
    if managed() { None } else { get(name) }
}

/// Resolve through secretspec's keyring provider, once per process.
#[cfg(feature = "keyring")]
fn keyring(name: &str) -> Option<String> {
    if let Some(hit) = cache().get(name) {
        return hit.clone();
    }
    let value = spec(Some(name)).ok().and_then(|secrets| match secrets.resolve_named(name) {
        Ok(NamedResolution::Resolved(r)) => r.value.filter(|v| !v.is_empty()),
        _ => None,
    });
    cache().insert(name.to_string(), value.clone());
    value
}

/// No keyring in this build: the environment is the only source (ADR 0051).
#[cfg(not(feature = "keyring"))]
fn keyring(_name: &str) -> Option<String> {
    None
}

/// The endpoint key: the configured secret, then OpenRouter's usual name.
pub fn api_key(secret_name: &str) -> Option<String> {
    get(secret_name).or_else(|| get("OPENROUTER_API_KEY"))
}

/// The message when no endpoint key resolves; nothing is sent without one.
pub fn missing_key(secret_name: &str) -> String {
    if managed() {
        return format!(
            "no API key: {secret_name} is not set in this Deck task; Deck injects host credentials at the gateway"
        );
    }
    let mut names = vec![secret_name];
    if secret_name != "OPENROUTER_API_KEY" {
        names.push("OPENROUTER_API_KEY");
    }
    // An env-only build has no `secrets set` to suggest (ADR 0051).
    #[cfg(feature = "keyring")]
    let store_hint = format!(" (or construct secrets set {secret_name})");
    #[cfg(not(feature = "keyring"))]
    let store_hint = "";
    format!(
        "no API key: set {}{store_hint}; for a keyless local endpoint set api_key_secret = \"\"",
        names.join(" or ")
    )
}

/// A key from the environment alone: no keyring, no OpenRouter fallback. A
/// delegated child uses this, since the parent hands it exactly the secrets
/// its definition names, and anything else it could find is not its to use.
pub fn env_key(secret_name: &str) -> Option<String> {
    env_value(secret_name)
}

pub fn set(name: &str, value: &str) -> Result<()> {
    if managed() {
        return Err(anyhow!(HOST_OWNED));
    }
    #[cfg(feature = "keyring")]
    {
        let mut secrets = spec(Some(name))?;
        secrets.set_provider("keyring");
        secrets.set_text(name, value).map_err(|e| anyhow!("secretspec: {e}"))?;
        cache().insert(name.to_string(), Some(value.to_string()));
        Ok(())
    }
    #[cfg(not(feature = "keyring"))]
    {
        let _ = (name, value);
        Err(anyhow!(ENV_ONLY))
    }
}

/// Which provider supplied a secret. The value itself is never part of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Deck's placeholder from the environment; the host gateway injects the real value.
    Placeholder,
    /// A value from the environment on a host.
    Environment,
    /// A value from the OS keyring.
    Keyring,
    /// Nothing supplies it.
    Missing,
}

/// One known secret and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub source: Source,
}

pub fn status() -> Vec<Status> {
    KNOWN.iter().map(|(name, _)| Status { name: (*name).to_string(), source: source(name) }).collect()
}

fn source(name: &str) -> Source {
    if env_value(name).is_some() {
        return if managed() { Source::Placeholder } else { Source::Environment };
    }
    if managed() {
        Source::Missing
    } else if keyring(name).is_some() {
        Source::Keyring
    } else {
        Source::Missing
    }
}

/// One `secrets status` line: the name and its source, never the value. On a
/// host a resolved secret keeps the old `✓ NAME`; a Deck placeholder says so.
pub fn status_line(status: &Status) -> String {
    match status.source {
        Source::Placeholder => format!("✓ {} (placeholder from environment)", status.name),
        Source::Environment | Source::Keyring => format!("✓ {}", status.name),
        Source::Missing => format!("· {}", status.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_reads_only_the_environment() {
        // A name nothing sets: no keyring lookup, no OpenRouter fallback.
        assert_eq!(env_key("CONSTRUCT_TEST_UNSET_KEY"), None);
        assert_eq!(env_key(""), None);
    }

    #[test]
    fn only_a_set_and_non_falsey_marker_means_managed() {
        for off in [None, Some(""), Some("  "), Some("0"), Some("false"), Some("FALSE")] {
            assert!(!managed_value(off), "{off:?} must not mean a Deck guest");
        }
        for on in [Some("1"), Some("yes"), Some("true")] {
            assert!(managed_value(on), "{on:?} must mean a Deck guest");
        }
    }

    #[test]
    fn status_lines_name_the_source_and_never_a_value() {
        let line = |source| status_line(&Status { name: "OPENAI_API_KEY".into(), source });
        assert_eq!(line(Source::Placeholder), "✓ OPENAI_API_KEY (placeholder from environment)");
        assert_eq!(line(Source::Environment), "✓ OPENAI_API_KEY");
        assert_eq!(line(Source::Keyring), "✓ OPENAI_API_KEY");
        assert_eq!(line(Source::Missing), "· OPENAI_API_KEY");
    }

    /// The env-only build (ADR 0051): no keyring lookup, no store, and the
    /// facade still answers. Names nothing sets keep these tests off the
    /// developer's real environment, like `env_key_reads_only_the_environment`.
    #[cfg(not(feature = "keyring"))]
    mod env_only {
        use super::*;

        #[test]
        fn unset_names_resolve_to_nothing_without_a_keyring() {
            assert_eq!(keyring("CONSTRUCT_TEST_UNSET_KEY"), None);
            assert_eq!(get("CONSTRUCT_TEST_UNSET_KEY"), None);
            assert_eq!(source("CONSTRUCT_TEST_UNSET_KEY"), Source::Missing);
        }

        #[test]
        fn set_refuses_and_points_at_the_environment() {
            let err = set("CONSTRUCT_TEST_KEY", "v").unwrap_err();
            // A Deck guest refuses host-owned first; either way nothing stores.
            if !managed() {
                assert_eq!(err.to_string(), ENV_ONLY);
            }
            assert!(!missing_key("OPENAI_API_KEY").contains("secrets set"));
        }
    }
}
