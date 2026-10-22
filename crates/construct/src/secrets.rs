//! Secrets through secretspec. Construct declares its spec in Rust, so there is
//! no secretspec.toml to ship. Values come from the environment first, then the
//! OS keyring; `construct secrets set NAME` writes to the keyring.

use anyhow::{Result, anyhow};
use secretspec::{NamedResolution, Secret, Secrets, Spec};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub const KNOWN: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "Key for the OpenAI-compatible model endpoint"),
    ("OPENROUTER_API_KEY", "OpenRouter key, if you keep it under that name"),
    ("TYPESAFE_API_KEY", "TypeSafe key for the Jev router"),
    ("EXA_API_KEY", "Exa key for web_search (optional)"),
    ("BRAVE_API_KEY", "Brave Search key for web_search (optional)"),
];

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

static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();

/// Resolve one secret, once per process (misses are cached too). The
/// environment is checked directly first: it is the common case and costs
/// nothing, while the keyring may wake a D-Bus service.
pub fn get(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name)
        && !v.is_empty()
    {
        return Some(v);
    }
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(name) {
        return hit.clone();
    }
    let value = spec(Some(name)).ok().and_then(|secrets| match secrets.resolve_named(name) {
        Ok(NamedResolution::Resolved(r)) => r.value.filter(|v| !v.is_empty()),
        _ => None,
    });
    cache.lock().unwrap().insert(name.to_string(), value.clone());
    value
}

/// The endpoint key: the configured secret, then OpenRouter's usual name.
pub fn api_key(secret_name: &str) -> Option<String> {
    get(secret_name).or_else(|| get("OPENROUTER_API_KEY"))
}

/// A key from the environment alone: no keyring, no OpenRouter fallback. A
/// delegated child uses this, since the parent hands it exactly the secrets
/// its definition names, and anything else it could find is not its to use.
pub fn env_key(secret_name: &str) -> Option<String> {
    std::env::var(secret_name).ok().filter(|v| !v.is_empty())
}

pub fn set(name: &str, value: &str) -> Result<()> {
    let mut secrets = spec(Some(name))?;
    secrets.set_provider("keyring");
    secrets.set_text(name, value).map_err(|e| anyhow!("secretspec: {e}"))?;
    if let Some(c) = CACHE.get() {
        c.lock().unwrap().insert(name.to_string(), Some(value.to_string()));
    }
    Ok(())
}

pub fn status() -> Vec<(String, bool)> {
    KNOWN.iter().map(|(n, _)| (n.to_string(), get(n).is_some())).collect()
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
}
