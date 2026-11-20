//! Read the settled generation record on the host using the benchmark credential.

use anyhow::{Context, Result, ensure};
use reqwest::{StatusCode, redirect::Policy};
use std::time::Duration;

/// Retrieve a settled receipt from OrcaRouter. Briefly retry unsettled records.
///
/// # Errors
///
/// Returns an error for an invalid ID, unavailable credential, or unreadable receipt.
pub async fn orca_receipt(id: &str, key_secret: Option<&str>, key_env: Option<&str>) -> Result<String> {
    ensure!(
        !id.is_empty() && id.len() <= 200 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "invalid OrcaRouter receipt ID"
    );
    let key = match key_env {
        Some(name) => std::env::var(name).context("missing OrcaRouter credential")?,
        None => super::resolve_key(key_secret.unwrap_or("ORCAROUTER_API_KEY"))?,
    };
    ensure!(key.starts_with("sk-orca-"), "expected an OrcaRouter credential");
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder().redirect(Policy::none()).timeout(Duration::from_secs(3)).build()?;
    for attempt in 0..5 {
        let response =
            client.get(format!("https://api.orcarouter.ai/v1/generation?id={id}")).bearer_auth(&key).send().await?;
        if response.status() != StatusCode::NOT_FOUND || attempt == 4 {
            ensure!(response.status().is_success(), "OrcaRouter receipt returned HTTP {}", response.status());
            return Ok(response.text().await?);
        }
        tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
    }
    anyhow::bail!("OrcaRouter receipt did not settle")
}
