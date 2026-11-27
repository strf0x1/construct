//! `construct hub model` (#390, ADR 0078): show, set or clear the model a
//! hub-proxied peer's calls use. The design choice: like `revoke`, it goes
//! through the running hub when there is one and edits the registry under
//! the instance lock when there is not, so the override is there either way.

use super::{ask, expect_ok, status};
use anyhow::{Result, bail};
use construct_core::config::Config;
use construct_hub::control::Request;
use construct_hub::{Files, offline};

/// `construct hub model`: show, set or clear `id`'s model override, through
/// the running hub or straight into the registry when none runs.
pub(super) async fn run(files: &Files, cfg: &Config, id: &str, model: Option<String>, clear: bool) -> Result<()> {
    if model.is_none() && !clear {
        let status = status(files).await?;
        let Some(p) = status.peers.iter().find(|p| p.peer.id == id) else { bail!("hub: no peer {id}") };
        match p.model() {
            Some((m, true)) => println!("{id}: {m} (set at the hub; `--clear` goes back to its own)"),
            Some((m, false)) => println!("{id}: {m} (its own)"),
            None => println!("{id}: its own model (it has made no model call through this hub yet)"),
        }
        return Ok(());
    }
    let req = Request::Model { id: id.to_string(), model: model.clone() };
    match ask(files, &req).await? {
        Some(resp) => expect_ok(Some(resp))?,
        None => offline::set_model(files, cfg, id, model.as_deref())?,
    }
    match model.as_deref().map(str::trim) {
        Some(m) => {
            let window = construct_hub::model::window(cfg, m)
                .map(|w| format!(" (window {w} tokens here; a larger context is compacted to fit)"))
                .unwrap_or_default();
            println!("hub: {id}'s model calls now go to {m}{window}");
        }
        None => println!("hub: {id} uses the model it asks for"),
    }
    Ok(())
}
