//! The one place a `reqwest` client is built (#290). Every construction site
//! in the workspace goes through `builder()`, so the crypto-provider install
//! — and, later, trust-root and proxy policy — is one edit here, not a hunt
//! across three crates. The presets name the recurring shapes; per-site knobs
//! (timeouts, pool idle, `tcp_nodelay`, redirects, retry) stay at the call
//! site, which is why these return a `ClientBuilder`, not a `Client`.
//!
//! `builder()` deliberately sets no user agent: the router evals, the dev
//! proxy and the learning gate send none today, and this module centralizes
//! policy byte-for-byte rather than changing it. `engine()` and `loopback()`
//! add the construct UA for the sites that already send it.

/// The UA the engine-facing clients send; the workspace shares one version.
const USER_AGENT: &str = concat!("construct/", env!("CARGO_PKG_VERSION"));

/// The policy site: install ring, the one crypto provider (#271), before the
/// first TLS client, then hand back a bare builder for the call site's knobs.
/// Once per process; an already-installed provider wins (`llm::ensure_crypto`).
pub fn builder() -> reqwest::ClientBuilder {
    crate::llm::ensure_crypto();
    reqwest::Client::builder()
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
