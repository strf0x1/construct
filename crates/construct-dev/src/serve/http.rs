//! The proxy's upstream client. ring is the workspace's one crypto provider
//! (#271); `construct_core::http` installs it, because the e2e harness runs
//! `serve` in-process, without the binary's startup.

use std::time::Duration;

/// `orca` disables redirects and retries: a benchmark admission accounts for
/// one dispatch, and each attempt passes through the harness's guard.
pub fn client(orca: bool) -> anyhow::Result<reqwest::Client> {
    let mut http = construct_core::http::builder().pool_idle_timeout(Duration::from_secs(120));
    if orca {
        http = http.redirect(reqwest::redirect::Policy::none()).retry(reqwest::retry::never());
    }
    construct_core::http::build(http)
}
