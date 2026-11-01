//! A client for a model other than the session's: the compaction summarizer
//! or the learning pass may name one on another provider (ADR 0027).
//!
//! The design choice: the session's client is reused whenever the other
//! model's endpoint is the same one, so a summarizer on the same provider
//! costs no new connection, and only a different endpoint asks for its key.

use super::Client;
use crate::config::Config;
use anyhow::Result;

impl Client {
    /// The client and wire id for `selector`: this client when `selector` is
    /// served by `cfg`'s own endpoint, else a new one with its own key.
    pub fn for_model(&self, cfg: &Config, selector: &str) -> Result<(Client, String)> {
        let ep = cfg.endpoint_for(selector);
        let client = if ep == cfg.endpoint() { self.clone() } else { Client::new(&ep, ep.require(ep.key())?)? };
        Ok((client, cfg.wire_id(selector)))
    }
}
