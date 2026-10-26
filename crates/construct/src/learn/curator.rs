//! The curator: once the agent owns enough unpinned skills, one LLM pass
//! merges, retires and re-describes them, so the library stays small enough
//! to route over.
use super::prompts::CURATOR;
use super::{Outcome, learner, run_loop};
use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use crate::memory::Store;
use crate::tools::Group;
use anyhow::Result;
use std::fmt::Write as _;

pub async fn curate(cfg: &Config, paths: &Paths) -> Result<Option<Outcome>> {
    let key = crate::secrets::api_key(&cfg.api_key_secret);
    let client = llm::Client::new(cfg, key)?;
    let memory = Store::new(paths, &cfg.memory);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let (ctx, catalog) = learner(cfg, paths, memory, rx)?;
    let usage = ctx.skills.usage();
    let now = crate::skills::now();
    let mut listing = String::new();
    let mut n = 0;
    for s in ctx.skills.list() {
        let u = usage.get(&s.name).cloned().unwrap_or_default();
        if u.author != "agent" || u.pinned {
            continue;
        }
        n += 1;
        let idle = if u.last_used == 0 {
            "never".to_string()
        } else {
            format!("{}d ago", (now.saturating_sub(u.last_used)) / 86_400)
        };
        let _ = writeln!(
            listing,
            "- {} (suggested {}, views {}, patches {}, last used {idle}): {}",
            s.name, u.suggested, u.views, u.patches, s.description
        );
    }
    if n < 3 {
        return Ok(None);
    }
    let defs = catalog.definitions(&[Group::Skills, Group::SkillWrite]);
    let out = run_loop(
        &client,
        cfg.learn_model(),
        &ctx,
        &catalog,
        vec![Message::system(CURATOR), Message::user(listing)],
        &defs,
        16,
    )
    .await?;
    Ok(Some(out))
}
