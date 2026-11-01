//! Detached learning command and its quiet-session debounce. Learning reads
//! history without holding the session writer ownership.

use crate::config::{Config, Paths};
use crate::{journal, learn, skills};
use anyhow::{Result, bail};

pub(super) async fn run(cfg: Config, paths: Paths, session: Option<String>, curate: bool) -> Result<()> {
    if let Some(id) = session {
        // Debounce: a session continued with `-c` spawns a learner per turn.
        // Wait until the session has gone quiet (its last seq unchanged) so
        // only the last learner reviews, and it never competes with a running
        // turn for the model.
        let db = journal::Journal::open(&journal::path(&paths, &std::env::current_dir()?))?;
        let quiet = std::env::var("CONSTRUCT_LEARN_DEBOUNCE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000u64);
        if quiet > 0 {
            let before = db.last_seq(&id)?;
            tokio::time::sleep(std::time::Duration::from_millis(quiet)).await;
            if db.last_seq(&id)? != before {
                learn::log_line(&paths, &format!("session {id}: still active; leaving it to the next learner"));
                return Ok(());
            }
        }
        let all = db.since(&id, 0)?;
        if all.is_empty() {
            bail!("no session {id} in this project's journal");
        }
        let msgs = journal::project(all.iter().map(|e| &e.event));
        let last = all.last().map(|e| e.seq).unwrap_or(0);
        let user_turns = msgs.iter().filter(|m| m.role == "user").count() as u32;
        let tool_calls = msgs.iter().map(|m| m.tool_calls.len() as u32).sum();
        let mut st = learn::state(&paths);
        // A session may already have been reviewed in-process up to some seq.
        let reviewed = st["reviewed_seq"][&id].as_i64().unwrap_or(0);
        let fresh = journal::project(db.since(&id, reviewed)?.iter().map(|e| &e.event));
        if reviewed < last && !fresh.is_empty() && learn::kind_for(user_turns, tool_calls).is_some() {
            // The curator below still runs either way: it tends agent-written
            // global skills, whatever strategy wrote them.
            let distilled = learn::distill(&cfg, &paths, &id, &fresh).await;
            if let Some(r) = &distilled {
                match r {
                    Ok(o) => {
                        learn::log_line(&paths, &format!("session {id}: {} | {}", o.summary, o.actions.join("; ")))
                    }
                    Err(e) => learn::log_line(&paths, &format!("session {id}: {} failed: {e:#}", cfg.learn.strategy)),
                }
            }
            // A cheap check first: most sessions hold nothing worth a review.
            let gate = match distilled {
                Some(_) => learn::Gate { kind: None, source: "", note: String::new() },
                None => {
                    let g = learn::gate(&cfg, &fresh).await;
                    learn::log_line(&paths, &format!("session {id}: gate {} {} -> {:?}", g.source, g.note, g.kind));
                    g
                }
            };
            if let Some(kind) = gate.kind {
                match learn::review(&cfg, &paths, &id, &fresh, kind).await {
                    Ok(o) => {
                        learn::log_line(&paths, &format!("session {id}: {} | {}", o.summary, o.actions.join("; ")));
                        println!("{}", o.summary);
                        for a in o.actions {
                            println!("  {a}");
                        }
                    }
                    Err(e) => learn::log_line(&paths, &format!("session {id}: review failed: {e:#}")),
                }
            }
            st["reviewed_seq"][&id] = serde_json::json!(last);
            learn::save_state(&paths, &st);
        }
    }
    let st = learn::state(&paths);
    let last = st["curated_at"].as_u64().unwrap_or(0);
    let due = skills::now().saturating_sub(last) > cfg.learn.curator_every_days as u64 * 86_400;
    if curate || due {
        if let Some(o) = learn::curate(&cfg, &paths).await? {
            learn::log_line(&paths, &format!("curator: {} | {}", o.summary, o.actions.join("; ")));
            println!("curator: {}", o.summary);
        }
        let mut st = learn::state(&paths);
        st["curated_at"] = serde_json::json!(skills::now());
        learn::save_state(&paths, &st);
    }
    Ok(())
}
