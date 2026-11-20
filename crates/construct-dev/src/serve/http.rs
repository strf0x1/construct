//! The proxy's upstream client. ring is the workspace's one crypto provider
//! (#271); the install lives here, not at the binary entry, because the e2e
//! harness runs `serve` in-process.

use std::time::Duration;

/// `orca` disables redirects and retries: a benchmark admission accounts for
/// one dispatch, and each attempt passes through the harness's guard.
pub fn client(orca: bool) -> anyhow::Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut http = reqwest::Client::builder().pool_idle_timeout(Duration::from_secs(120));
    if orca {
        http = http.redirect(reqwest::redirect::Policy::none()).retry(reqwest::retry::never());
    }
    Ok(http.build()?)
}
