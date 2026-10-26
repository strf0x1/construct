//! The observation ledger: what a session taught, appended as JSONL and kept
//! until the same problem *class* recurs in another session.
use super::digest;
use super::prompts::OBSERVE;
use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use anyhow::Result;
use serde_json::{Value, json};

/// One thing a session taught, kept until it recurs. `problem_class` is what
/// recurrence is matched on: the kind of problem, not the task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct Observation {
    pub ts: u64,
    pub session: String,
    pub cwd: String,
    pub problem_class: String,
    pub summary: String,
    pub fix: String,
    pub evidence: String,
}

fn ledger_path(paths: &Paths) -> std::path::PathBuf {
    paths.home.join("learn").join("ledger.jsonl")
}

pub fn ledger(paths: &Paths) -> Vec<Observation> {
    std::fs::read_to_string(ledger_path(paths))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub(super) fn append_ledger(paths: &Paths, obs: &[Observation]) {
    use std::io::Write;
    if obs.is_empty() {
        return;
    }
    let p = ledger_path(paths);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        for o in obs {
            let _ = writeln!(f, "{}", serde_json::to_string(o).unwrap_or_default());
        }
    }
}

pub(super) async fn observe(
    client: &llm::Client,
    model: &str,
    messages: &[Message],
    session: &str,
) -> Result<Vec<Observation>> {
    let msgs =
        vec![Message::system(OBSERVE), Message::user(format!("<session>\n{}\n</session>", digest(messages, 40_000)))];
    let c = client
        .complete(llm::Request { model, messages: &msgs, tools: &[], max_tokens: Some(1500), temperature: None })
        .await?;
    let text = c.message.content.unwrap_or_default();
    let json_text = text.find('{').and_then(|a| text.rfind('}').map(|b| &text[a..=b])).unwrap_or("{}");
    let v: Value = serde_json::from_str(json_text).unwrap_or(json!({}));
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let ts = crate::skills::now();
    Ok(v["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let class = o["problem_class"].as_str()?.trim();
            (!class.is_empty()).then(|| Observation {
                ts,
                session: session.to_string(),
                cwd: cwd.clone(),
                problem_class: llm::truncate(class, 80),
                summary: llm::truncate(o["summary"].as_str().unwrap_or(""), 300),
                fix: llm::truncate(o["fix"].as_str().unwrap_or(""), 600),
                evidence: llm::truncate(o["evidence"].as_str().unwrap_or(""), 400),
            })
        })
        .take(5)
        .collect())
}

/// For each new observation, the earlier ones (from other sessions) that are
/// the same class of problem. Jev decides when a key resolves; word overlap
/// on the problem class otherwise.
pub(super) async fn recurrences(
    cfg: &Config,
    new: &[Observation],
    prior: &[Observation],
) -> Vec<(Observation, Vec<Observation>)> {
    let mut out = Vec::new();
    for o in new {
        let others: Vec<&Observation> = prior.iter().filter(|p| p.session != o.session).rev().take(60).collect();
        if others.is_empty() {
            continue;
        }
        let jev = match crate::secrets::get("TYPESAFE_API_KEY")
            .filter(|_| cfg.router.mode != "local" && cfg.router.mode != "off")
        {
            Some(key) => same_problem_jev(cfg, &key, o, &others).await.ok(),
            None => None,
        };
        let matched: Vec<Observation> = match jev {
            Some(idx) => idx.into_iter().map(|i| others[i].clone()).collect(),
            None => others
                .iter()
                .filter(|p| {
                    p.problem_class.eq_ignore_ascii_case(&o.problem_class)
                        || crate::router::overlap(&o.problem_class, &p.problem_class) >= 0.5
                })
                .map(|p| (*p).clone())
                .collect(),
        };
        if !matched.is_empty() {
            out.push((o.clone(), matched));
        }
    }
    out
}

async fn same_problem_jev(
    cfg: &Config,
    key: &str,
    o: &Observation,
    others: &[&Observation],
) -> Result<Vec<usize>, String> {
    let mut criteria = serde_json::Map::new();
    // "none" first: Jev favours the first option (see src/router.rs).
    criteria.insert("none".into(), json!("none of these is the same kind of problem"));
    for (i, p) in others.iter().enumerate() {
        criteria.insert(format!("o{i}"), json!(format!("{}: {}", p.problem_class, llm::truncate(&p.summary, 200))));
    }
    let mut q = serde_json::Map::new();
    q.insert(
        "same".into(),
        json!({ "type": "choice", "instructions": "Which earlier observation is the same kind of problem as `new` (would be solved by the same procedure), if any?", "criteria": criteria }),
    );
    let state = json!({ "new": format!("{}: {}", o.problem_class, o.summary) });
    let v = crate::jev::ask(
        &cfg.router,
        &reqwest::Client::new(),
        key,
        "recurrence",
        state,
        q,
        std::time::Duration::from_secs(5),
    )
    .await?;
    let probs = v["answers"]["same"]["probabilities"].as_object().cloned().unwrap_or_default();
    Ok(probs
        .iter()
        .filter(|(k, p)| *k != "none" && p.as_f64().unwrap_or(0.0) >= 0.4)
        .filter_map(|(k, _)| k[1..].parse().ok())
        .collect())
}
