//! Resolve a named proxy credential in Construct's SecretSpec namespace.
//! Errors, including agent audit requirements, propagate before the proxy listens.

use anyhow::{Context, Result, anyhow, bail};
use secretspec::{NamedResolution, Secret, Secrets, Spec};

/// Read only the declared upstream secret, without OpenRouter credential fallback.
pub fn resolve_key(name: &str) -> Result<String> {
    let spec = Spec::builder("construct")
        .default_providers(["env", "keyring"])
        .secret(name, Secret::optional("Benchmark upstream API key"))
        .build()
        .map_err(|e| anyhow!("secretspec: {e}"))?;
    let secrets = Secrets::from_spec(spec)?;
    if let NamedResolution::Resolved(value) =
        secrets.resolve_named(name).with_context(|| format!("resolving upstream secret {name}"))?
    {
        return value.value.filter(|s| !s.is_empty()).with_context(|| format!("no upstream key {name}"));
    }
    bail!("no upstream key {name}; set it with construct secrets set")
}
