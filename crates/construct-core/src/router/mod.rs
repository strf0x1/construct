//! Decide, per user turn, which tool groups, MCP servers and skills are worth
//! putting in front of the model, so it never has to choose from a huge list.
//!
//! Jev (TypeSafe's System One model) answers this as one request of typed
//! questions: a noul per optional group, a noul per MCP server, and a choice
//! over the skill library. Without a TypeSafe key, or when Jev is slow or
//! down, a local lexical router answers instead (`local.rs`).
//!
//! Only groups the session's catalog has are asked about or returned: a
//! build without a tool crate never routes to its empty group (#271).

use crate::config::RouterConfig;
use crate::skills::Skill;
use crate::tools::Group;
use local::local;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

mod local;

#[derive(Debug, Clone, Default)]
pub struct Route {
    pub groups: Vec<Group>,
    /// Suggested skills with their probability, best first.
    pub skills: Vec<(String, f32)>,
    pub source: String,
    pub ms: u64,
    pub note: String,
}

pub struct Input<'a> {
    pub request: &'a str,
    /// The previous assistant message, for follow-ups like "do the same for X".
    pub previous: &'a str,
    pub skills: &'a [Skill],
    pub mcp: &'a BTreeMap<String, String>,
    /// The built-in groups this session's catalog has (`Catalog::groups`).
    pub groups: &'a BTreeSet<Group>,
}

impl Input<'_> {
    /// A group this session could switch on: one its catalog has, or an MCP
    /// server's, whose tools join the catalog when it starts.
    fn offers(&self, g: &Group) -> bool {
        matches!(g, Group::Mcp(_)) || self.groups.contains(g)
    }
}

pub async fn route(cfg: &RouterConfig, http: &reqwest::Client, input: Input<'_>) -> Route {
    let started = Instant::now();
    let mode = cfg.mode.as_str();
    let mut route = match mode {
        "off" => Route { groups: all_groups(&input), source: "off".into(), ..Default::default() },
        "local" => local(cfg, &input),
        _ => match (mode, crate::secrets::optional("TYPESAFE_API_KEY")) {
            (_, Some(key)) => match jev(cfg, http, &key, &input).await {
                Ok(r) => r,
                Err(e) => {
                    let mut r = local(cfg, &input);
                    r.note = format!("jev failed ({e}); used local router");
                    r
                }
            },
            ("jev", None) => {
                let mut r = local(cfg, &input);
                r.note = "TYPESAFE_API_KEY not set; used local router".into();
                r
            }
            _ => local(cfg, &input),
        },
    };
    route.groups.retain(|g| input.offers(g));
    route.ms = started.elapsed().as_millis() as u64;
    route
}

/// Open the connection to Jev ahead of the first turn when Jev will route,
/// so the first route doesn't pay for the TLS handshake (~100-150ms).
pub fn warm(cfg: &RouterConfig, http: &reqwest::Client) {
    if cfg.mode == "local" || cfg.mode == "off" {
        return;
    }
    let (http, url) = (http.clone(), cfg.jev_url.clone());
    tokio::spawn(async move {
        if crate::secrets::optional("TYPESAFE_API_KEY").is_some() {
            let _ = http.head(url).timeout(Duration::from_secs(5)).send().await;
        }
    });
}

fn all_groups(input: &Input<'_>) -> Vec<Group> {
    let mut g = vec![Group::Web, Group::Memory, Group::History, Group::SkillWrite];
    if !input.skills.is_empty() {
        g.push(Group::Skills);
    }
    g.extend(input.mcp.keys().map(|k| Group::Mcp(k.clone())));
    g
}

// One noul per group, each phrased strictly: "explicitly require ...; answer no
// if the local files and a shell suffice". Loosely worded nouls ("would X help
// with `request`?") read literally and fired on nearly everything (12/47 on
// bench/jev); strict ones score 43/47, ahead of a single choice (40/47), with
// no option-order bias and room for several groups at once. Wording borrowed
// from thruwire/foreman. See bench/jev/.
const NO_UNLESS: &str =
    "Answer no if it can be done with only the local files and a shell, or if the evidence is insufficient.";
const Q_WEB: &str = "the internet: web search, fetching URLs or online docs, checking current versions, CVEs or news";
const Q_HISTORY: &str =
    "transcripts of earlier conversations with the assistant (what we did or decided in a past session)";
const Q_MEMORY: &str = "Does `request` explicitly tell the assistant how to behave in future sessions, or share a lasting fact about the user or their machine? Answer no for ordinary task requests.";
const Q_SKILL_WRITE: &str = "Does `request` explicitly ask to create, change or delete one of the assistant's saved skills? Answer no otherwise.";
const Q_NEEDS_SKILL: &str = "Would a written procedure for a specific kind of task help with `request` (as opposed to a quick question or casual chat)?";

async fn jev(cfg: &RouterConfig, http: &reqwest::Client, key: &str, input: &Input<'_>) -> Result<Route, String> {
    let mut questions = Map::new();
    let noul = |instructions: String| json!({ "type": "noul", "instructions": instructions });
    let needs = |what: &str| noul(format!("Does `request` explicitly require {what}? {NO_UNLESS}"));
    let builtin = [
        ("web", Group::Web, needs(Q_WEB)),
        ("history", Group::History, needs(Q_HISTORY)),
        ("memory", Group::Memory, noul(Q_MEMORY.into())),
        ("skill_write", Group::SkillWrite, noul(Q_SKILL_WRITE.into())),
    ];
    let mut asked: Vec<(String, Group)> = vec![];
    for (k, g, q) in builtin.into_iter().filter(|(_, g, _)| input.offers(g)) {
        questions.insert(k.into(), q);
        asked.push((k.into(), g));
    }
    for (name, desc) in input.mcp {
        questions
            .insert(format!("mcp:{name}"), needs(&format!("the {name} tools: {}", crate::text::truncate(desc, 300))));
    }
    // Choice allows at most 255 options; keep the best-matching 254 + none.
    let mut candidates: Vec<&Skill> = input.skills.iter().collect();
    if candidates.len() > 254 {
        let mut scored: Vec<(f32, &Skill)> =
            candidates.iter().map(|s| (overlap(input.request, &skill_text(s)), *s)).collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        candidates = scored.into_iter().take(254).map(|(_, s)| s).collect();
    }
    if !candidates.is_empty() {
        questions.insert("needs_skill".into(), noul(Q_NEEDS_SKILL.into()));
        let mut criteria = Map::new();
        for s in &candidates {
            criteria.insert(s.name.clone(), json!(crate::text::truncate(&s.description, 300)));
        }
        criteria.insert("none".into(), json!("No skill here fits the request"));
        questions.insert(
            "skill".into(),
            json!({ "type": "choice", "instructions": "Which skill's procedure fits `request` best?", "criteria": criteria }),
        );
    }
    let mut state = json!({ "request": crate::text::truncate(input.request, 4000) });
    if !input.previous.is_empty() {
        state["previous_reply"] = json!(crate::text::truncate(input.previous, 600));
    }
    let v = crate::jev::ask(cfg, http, key, "route", state, questions, Duration::from_millis(2500)).await?;
    let answers = &v["answers"];
    let p = |k: &str| answers[k]["noul"].as_f64().unwrap_or(0.0) as f32;
    let t = cfg.threshold;
    let mut groups = Vec::new();
    let mut note = Vec::new();
    asked.extend(input.mcp.keys().map(|n| (format!("mcp:{n}"), Group::Mcp(n.clone()))));
    for (k, g) in asked {
        note.push(format!("{k}={:.2}", p(&k)));
        if p(&k) >= t {
            groups.push(g);
        }
    }
    let mut skills = Vec::new();
    if let Some(probs) = answers["skill"]["probabilities"].as_object() {
        let gate = p("needs_skill");
        note.push(format!("needs_skill={gate:.2}"));
        let none = probs.get("none").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let mut ranked: Vec<(String, f32)> = probs
            .iter()
            .filter(|(k, _)| *k != "none")
            .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0) as f32))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        // Suggest when the turn wants a procedure and a skill beats "none",
        // or one skill is a clear favourite regardless.
        for (name, prob) in ranked.into_iter().take(3) {
            if (gate >= 0.3 && prob > none.min(0.5) && prob >= 0.15) || prob >= 0.6 {
                skills.push((name, prob));
            }
        }
    }
    if !skills.is_empty() || groups.contains(&Group::SkillWrite) {
        groups.push(Group::Skills);
    }
    Ok(Route { groups, skills, source: v["model"].as_str().unwrap_or("jev").to_string(), ms: 0, note: note.join(" ") })
}

fn skill_text(s: &Skill) -> String {
    format!("{} {}", s.name.replace('-', " "), s.description)
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOP.contains(w))
        .map(|w| w.trim_end_matches('s').to_string())
        .collect()
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "you", "your", "are", "can", "how", "what", "when", "use", "from",
    "into", "not", "all", "any", "but", "have", "has", "was", "will", "about", "then", "them", "their", "its", "our",
    "out", "get", "set", "make", "please", "want", "need", "like", "just",
];

/// Share of the skill's words that appear in the request, weighted toward
/// rarer (longer) words. Crude, fast, and good enough as a fallback.
pub(crate) fn overlap(request: &str, doc: &str) -> f32 {
    let r: std::collections::HashSet<String> = words(request).into_iter().collect();
    let d = words(doc);
    if d.is_empty() || r.is_empty() {
        return 0.0;
    }
    let total: f32 = d.iter().map(|w| w.len() as f32).sum();
    let hit: f32 = d.iter().filter(|w| r.contains(*w)).map(|w| w.len() as f32).sum();
    hit / total
}
