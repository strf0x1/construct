//! A peer's model override (#390, ADR 0078): the selector the hub uses in
//! place of the one an `llm = "hub"` peer sends, set with `construct hub
//! model` and kept on the peer's registry row.
//!
//! The design choice: the peer sizes its context from its own catalog entry
//! for the selector it was stamped with, so an override to a smaller model
//! could overflow. The hub does not tell the peer anything new. It answers a
//! request that cannot fit the override's window with the same rejection a
//! provider sends ("maximum context length is N tokens"). The peer's
//! overflow recovery already reads that: it compacts, then keeps the smaller
//! window for the rest of the session. The provider's own rejection does the
//! same for anything this check lets through.

use construct_core::config::Config;
use construct_core::models::WindowSource;

/// Bytes per token for the size check. Prose runs about 4 bytes a token and
/// JSON or code fewer, so this underestimates: the hub refuses only what
/// cannot fit, and the provider judges the rest.
const BYTES_PER_TOKEN: u64 = 4;

/// `selector` as the hub keeps it: trimmed, and one the host's config can
/// route (a selector two providers both catalogue is refused).
pub fn check(cfg: &Config, selector: &str) -> Result<String, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("the model selector is empty; `--clear` removes an override".into());
    }
    cfg.resolve(selector).map_err(|e| format!("{e:#}"))?;
    Ok(selector.to_string())
}

/// The window `selector` has in the host's config, when something there
/// sets one. With nothing set, the hub knows no window and checks nothing.
#[must_use]
pub fn window(cfg: &Config, selector: &str) -> Option<u64> {
    let mut cfg = cfg.clone();
    cfg.select_model(selector).ok()?;
    (cfg.window_source() != WindowSource::Default).then_some(u64::from(cfg.context_window))
}

/// The provider-shaped rejection for a request body of `bytes` that cannot
/// fit `window`, or `None` when it may.
#[must_use]
pub fn too_big(bytes: usize, window: u64, selector: &str) -> Option<String> {
    let tokens = bytes as u64 / BYTES_PER_TOKEN;
    // "about" keeps the estimate out of the peer's bytes-per-token measure.
    (tokens > window).then(|| {
        format!(
            "This model's maximum context length is {window} tokens. However, your messages resulted in about \
             {tokens} tokens (hub: this peer's model is set to {selector} at the hub)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use construct_core::compact::{is_overflow, overflow_limit, overflow_tokens};

    #[test]
    fn the_rejection_is_one_the_peers_overflow_recovery_reads() {
        assert_eq!(too_big(4_000, 1_000, "small"), None, "1000 tokens fit 1000");
        let why = too_big(4_004, 1_000, "small").unwrap();
        let err = format!("model endpoint returned 400 Bad Request: {why}");
        assert!(is_overflow(&err), "{err}");
        assert_eq!(overflow_limit(&err), Some(1_000));
        assert_eq!(overflow_tokens(&err), None, "an estimate must not calibrate the peer");
    }

    #[test]
    fn a_window_is_known_only_when_the_hosts_config_sets_one() {
        let cfg: Config =
            toml::from_str("[models.small]\ncontext_window = 8000\n[models.plain]\nmax_tokens = 100\n").unwrap();
        assert_eq!(window(&cfg, "small"), Some(8_000));
        assert_eq!(window(&cfg, "plain"), None);
        assert_eq!(window(&cfg, "unknown"), None);
        assert_eq!(check(&cfg, " small ").unwrap(), "small");
        assert!(check(&cfg, "  ").is_err());
    }
}
