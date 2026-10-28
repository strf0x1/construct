//! `[learn] strategy = "workflow"`: what worked, as a procedure, after Agent
//! Workflow Memory (Wang et al. 2024) and self-generated exemplars (Sarukkai
//! et al. 2025). After a session that ended in a verified working state, one
//! call induces the steps that got there, with task-specific values replaced
//! by placeholders, and merges them into this repo's few workflows. No lesson
//! is distilled into advice; the agent's own successful trajectory is the
//! lesson. Workflows are per project: they describe one repo's mechanics.
use super::{Outcome, ask_json, digest, evidence};
use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const FILE: &str = "WORKFLOWS.json";
const MAX: usize = 3;
const MAX_STEPS: usize = 10;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Workflow {
    pub title: String,
    pub steps: Vec<String>,
}

pub fn path(kit: &Path) -> PathBuf {
    kit.join(FILE)
}

pub fn load(kit: &Path) -> Vec<Workflow> {
    std::fs::read_to_string(path(kit)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn prompt_block(kit: Option<&Path>) -> String {
    let Some(kit) = kit else { return String::new() };
    let wfs = load(kit);
    if wfs.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n# Workflows that worked in this repo before (follow the one that fits)\n");
    for w in wfs {
        let _ = writeln!(s, "## {}", w.title);
        for (i, step) in w.steps.iter().enumerate() {
            let _ = writeln!(s, "{}. {}", i + 1, step.replace('\n', " "));
        }
    }
    s
}

const SYSTEM: &str = "You keep a few reusable workflows for one repo: the steps and exact commands that got a task done there. You read a finished session and the current workflows, and reply with JSON only.
- First decide: did the session end in a verified working state (tests or a run passed after the last change, and the user didn't report a problem afterwards)? If not, reply {\"verified\": false}.
- If it did, induce the workflow it followed: the steps a future task of the same kind needs, in order, with exact commands. Replace task-specific names and values with {placeholders}. Keep steps the user required (conventions, setup they had to tell you about) and the command that worked where another failed; drop dead ends, exploration and anything a retry fixed.
- Merge it into the most similar existing workflow rather than adding a near-duplicate. At most 3 workflows, each at most 10 steps.
Reply exactly: {\"verified\": true, \"workflows\": [{\"title\": \"Add a CLI command\", \"steps\": [\"...\"]}]} with the full new set.";

/// The new set, if the reply was a verified one worth keeping.
pub fn parse(v: &serde_json::Value) -> Option<Vec<Workflow>> {
    if v["verified"].as_bool() != Some(true) {
        return None;
    }
    let mut wfs: Vec<Workflow> = serde_json::from_value(v["workflows"].clone()).ok()?;
    wfs.retain(|w| !w.title.trim().is_empty() && !w.steps.is_empty());
    for w in &mut wfs {
        w.title = llm::truncate(w.title.trim(), 80);
        w.steps.truncate(MAX_STEPS);
        for s in &mut w.steps {
            *s = llm::truncate(s.trim(), 300);
        }
    }
    wfs.truncate(MAX);
    (!wfs.is_empty()).then_some(wfs)
}

pub async fn induce(cfg: &Config, paths: &Paths, messages: &[Message]) -> Result<Outcome> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(kit) = crate::project::detect(paths, &cwd) else {
        return Ok(Outcome { summary: "Not in a repo; workflows are per repo.".into(), actions: vec![] });
    };
    let client = llm::Client::new(cfg, cfg.endpoint_key())?;
    let current = load(&kit.dir);
    let mut task = format!("<session>\n{}\n</session>\n\n", digest(messages, 40_000));
    task.push_str(&evidence::section(messages));
    let _ = writeln!(task, "\nCURRENT WORKFLOWS:\n{}", serde_json::to_string_pretty(&current).unwrap_or_default());
    let v = ask_json(&client, cfg.learn_model(), SYSTEM, &task, 1500).await?;
    let Some(wfs) = parse(&v) else {
        return Ok(Outcome { summary: "Nothing to save (not a verified success).".into(), actions: vec![] });
    };
    if wfs == current {
        return Ok(Outcome { summary: "Nothing to save.".into(), actions: vec![] });
    }
    kit.touch();
    let _ =
        crate::config::write_atomic(&path(&kit.dir), serde_json::to_string_pretty(&wfs).unwrap_or_default().as_bytes());
    let titles: Vec<String> = wfs.iter().map(|w| format!("{} ({} steps)", w.title, w.steps.len())).collect();
    Ok(Outcome { summary: "Workflows updated.".into(), actions: vec![format!("workflows: {}", titles.join("; "))] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_unverified_session_saves_nothing() {
        assert!(parse(&json!({"verified": false})).is_none());
        assert!(parse(&json!({"verified": true, "workflows": []})).is_none());
    }

    #[test]
    fn caps_workflows_and_steps() {
        let steps: Vec<String> = (0..15).map(|i| format!("step {i}")).collect();
        let wfs: Vec<_> = (0..5).map(|i| json!({"title": format!("w{i}"), "steps": steps})).collect();
        let got = parse(&json!({"verified": true, "workflows": wfs})).unwrap();
        assert_eq!(got.len(), MAX);
        assert_eq!(got[0].steps.len(), MAX_STEPS);
    }
}
