//! The one place a `reqwest` client is built (#290). Every construction site
//! in the workspace goes through `builder()` and `build()`, so the
//! crypto-provider install, the trust roots and the actionable trust-store
//! error are one edit here, not a hunt across three crates. The presets name
//! the recurring shapes; per-site knobs (timeouts, pool idle, `tcp_nodelay`,
//! redirects, retry) stay at the call site, which is why these return a
//! `ClientBuilder`, not a `Client`.
//!
//! Trust roots (#293), sources merged rather than replaced:
//!
//! - `CONSTRUCT_CA_FILE` names an extra PEM bundle (or one DER certificate).
//!   It is merged with the platform store, never a replacement, so a private
//!   CA composes with a managed host's.
//! - Static musl builds bake Mozilla's roots in as *extra* roots too: those
//!   are the drop-in artifacts and may run where no certificate store exists
//!   (scratch, busybox). Merging keeps a native enterprise CA working.
//! - Everything else reads the platform store through `rustls-native-certs`,
//!   which honors `SSL_CERT_FILE` and `SSL_CERT_DIR`.
//!
//! When the platform store is empty and no extra root was configured,
//! `build()` replaces `builder error` with a message naming both knobs.
//!
//! `builder()` deliberately sets no user agent: the router evals, the dev
//! proxy and the learning gate send none today, and this module centralizes
//! policy byte-for-byte rather than changing it. `engine()` and `loopback()`
//! add the construct UA for the sites that already send it.

use anyhow::{Context, Result, anyhow};
use std::path::Path;

/// The UA the engine-facing clients send; the workspace shares one version.
const USER_AGENT: &str = concat!("construct/", env!("CARGO_PKG_VERSION"));

/// The one failure a rescuer can fix: the provider loaded an empty trust
/// store and no extra root was configured. `build()` prints exactly this.
const NO_ROOTS: &str = "no CA certificates found: set SSL_CERT_FILE=/path/to/ca-certificates.crt \
     (or CONSTRUCT_CA_FILE), or install ca-certificates";

/// Install ring, the one crypto provider (#271), before the first TLS client.
/// reqwest is built with `rustls-no-provider` to keep aws-lc-rs (a BoringSSL
/// fork) out of the binary. The engine is a library: its tests and embedders
/// build clients without the binary's startup, so `builder()` calls
/// this for every construction site. Once per process; an already-installed
/// provider wins.
pub fn ensure_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The policy site: install ring, the one crypto provider (#271), before the
/// first TLS client, then hand back a bare builder for the call site's knobs.
/// Once per process; an already-installed provider wins (`ensure_crypto`).
#[expect(
    clippy::disallowed_methods,
    reason = "the one legitimate reqwest constructor: every other site goes through this helper (#290)"
)]
pub fn builder() -> reqwest::ClientBuilder {
    ensure_crypto();
    let builder = reqwest::Client::builder();
    // Only static builds need roots in the binary. A glibc or macOS build
    // runs where a trust store exists (or can point SSL_CERT_FILE at one);
    // the ~130 KB of DER is not spent there (ADR 0053).
    #[cfg(target_env = "musl")]
    let builder = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .fold(builder, |builder, cert| builder.add_root_certificate(certificate(cert.as_ref())));
    builder
}

/// An engine-facing client: the construct user agent on top of `builder()`.
pub fn engine() -> reqwest::ClientBuilder {
    builder().user_agent(USER_AGENT)
}

/// A Deck loopback gateway client. Loopback, in the guest for the GitHub
/// gateway: a proxy variable must not send it elsewhere, so `no_proxy()` is
/// explicit here — security-relevant, never a default to be inherited.
pub fn loopback() -> reqwest::ClientBuilder {
    engine().no_proxy()
}

/// Build the client, applying `CONSTRUCT_CA_FILE` and turning the one fatal
/// trust-store failure into an actionable message (#293).
///
/// The file is read here rather than in `builder()` because a bad path must
/// become an error, and `ClientBuilder` has no error channel.
pub fn build(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    let mut builder = builder;
    if let Some(path) = std::env::var_os("CONSTRUCT_CA_FILE") {
        for cert in ca_certs(Path::new(&path))? {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder.build().map_err(trust_error)
}

/// Read a CA bundle: every PEM block, or one DER certificate. Errors name the
/// file, so a typo in `CONSTRUCT_CA_FILE` is not a TLS mystery.
fn ca_certs(path: &Path) -> Result<Vec<reqwest::Certificate>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("CONSTRUCT_CA_FILE={}: could not read the file", path.display()))?;
    if let Ok(certs) = reqwest::Certificate::from_pem_bundle(&bytes)
        && !certs.is_empty()
    {
        return Ok(certs);
    }
    if bytes.first() == Some(&0x30)
        && let Ok(cert) = reqwest::Certificate::from_der(&bytes)
    {
        return Ok(vec![cert]);
    }
    Err(anyhow!("CONSTRUCT_CA_FILE={}: contains no PEM certificates (and is not a DER certificate)", path.display()))
}

#[cfg(target_env = "musl")]
fn certificate(der: &[u8]) -> reqwest::Certificate {
    // `from_der` only stores the bytes; a bad root fails client construction,
    // which the musl release smoke's real HTTPS fetch catches (ADR 0053).
    #[expect(
        clippy::expect_used,
        reason = "reqwest stores DER without validating it; the release smoke proves the set"
    )]
    reqwest::Certificate::from_der(der).expect("baked webpki root")
}

/// The empty-platform-store error arrives as an opaque `builder error`; walk
/// the source chain and replace it with the fix. Every other failure keeps its
/// original message and chain.
fn trust_error(e: reqwest::Error) -> anyhow::Error {
    if has_empty_store(&e) { anyhow!("{NO_ROOTS}") } else { anyhow::Error::new(e) }
}

/// The rustls message sits anywhere in the chain; find it by its text, which
/// is the only stable handle reqwest exposes on a builder error.
fn has_empty_store(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(e);
    while let Some(err) = source {
        if err.to_string().contains("No CA certificates were loaded from the system") {
            return true;
        }
        source = err.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Link(&'static str, Option<Box<Link>>);

    impl std::fmt::Display for Link {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Link {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|e| e as &(dyn std::error::Error + 'static))
        }
    }

    /// The empty-store message is what a rescuer sees; pin its wording and
    /// that unrelated failures pass through unchanged.
    #[test]
    fn only_the_empty_store_failure_gets_the_fix() {
        let empty = Link("builder error", Some(Box::new(Link("No CA certificates were loaded from the system", None))));
        assert!(has_empty_store(&empty));
        assert!(NO_ROOTS.contains("SSL_CERT_FILE") && NO_ROOTS.contains("CONSTRUCT_CA_FILE"));

        let other = Link("builder error", Some(Box::new(Link("invalid certificate", None))));
        assert!(!has_empty_store(&other));
    }

    #[test]
    fn a_missing_ca_file_names_the_variable_and_path() {
        let err = ca_certs(Path::new("/does/not/exist/ca.pem")).unwrap_err().to_string();
        assert!(err.contains("CONSTRUCT_CA_FILE=/does/not/exist/ca.pem"), "{err}");
    }

    #[test]
    fn a_file_without_certificates_is_rejected() {
        let path = std::env::temp_dir().join(format!("construct-ca-{}", std::process::id()));
        std::fs::write(&path, b"not a certificate").unwrap();
        let err = ca_certs(&path).unwrap_err().to_string();
        let _ = std::fs::remove_file(&path);
        assert!(err.contains("contains no PEM certificates"), "{err}");
    }
}
