//! Decide, per user turn, which tool groups, MCP servers and skills are worth
//! putting in front of the model, so it never has to choose from a huge list.
//!
//! Jev (TypeSafe's System One model) answers this as one request of typed
//! questions: a noul per optional group, a noul per MCP server, and a choice
//! over the skill library. Without a TypeSafe key, or when Jev is slow or
//! down, a local lexical router answers instead.

use crate::config::RouterConfig;
use crate::skills::Skill;
use crate::tools::Group;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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
}

pub async fn route(cfg: &RouterConfig, http: &reqwest::Client, input: Input<'_>) -> Route {
    let started = Instant::now();
    let mode = cfg.mode.as_str();
    let mut route = match mode {
        "off" => Route { groups: all_groups(&input), source: "off".into(), ..Default::default() },
        "local" => local(cfg, &input),
        _ => match (mode, crate::secrets::get("TYPESAFE_API_KEY")) {
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
        if crate::secrets::get("TYPESAFE_API_KEY").is_some() {
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

// Groups are asked as two choices rather than a yes/no per group: independent
// nouls read the request literally and fire on anything nearby (a GitHub noul
// was >0.6 for most coding requests), while a choice with an explicit "none"
// has to weigh the options against each other. Jev also favours whichever
// option comes first (+0.2-0.35 on ambiguous cases), so the conservative
// answer ("none", "task") always goes first; serde_json keeps insertion order.
// See bench/jev/.
const Q_ACCESS: &str = "Besides the local files and a shell, what does the assistant need to access to handle `request`?";
const Q_INTENT: &str = "What is `request` mainly doing?";
const Q_NEEDS_SKILL: &str = "Would a written procedure for a specific kind of task help with `request` (as opposed to a quick question or casual chat)?";

async fn jev(cfg: &RouterConfig, http: &reqwest::Client, key: &str, input: &Input<'_>) -> Result<Route, String> {
    let mut questions = Map::new();
    let noul = |instructions: String| json!({ "type": "noul", "instructions": instructions });
    let mut access = Map::new();
    access.insert("none".into(), json!("nothing beyond the local repository, files and shell commands"));
    access.insert("web".into(), json!("the internet: web search, fetching URLs or online docs, checking current versions, CVEs or news"));
    access.insert("history".into(), json!("transcripts of earlier conversations with the assistant (what we did or decided in a past session)"));
    for (name, desc) in input.mcp {
        access.insert(format!("mcp:{name}"), json!(format!("the {name} tools: {}", crate::llm::truncate(desc, 300))));
    }
    questions.insert("access".into(), json!({ "type": "choice", "instructions": Q_ACCESS, "criteria": access }));
    questions.insert(
        "intent".into(),
        json!({ "type": "choice", "instructions": Q_INTENT, "criteria": {
            "task": "asks the assistant to do or answer something (code, files, ops, questions)",
            "preference": "tells the assistant how to behave from now on, or shares a lasting fact about the user or their machine",
            "skill-edit": "asks to create, change or delete one of the assistant's saved skills",
        } }),
    );
    // Choice allows at most 255 options; keep the best-matching 254 + none.
    let mut candidates: Vec<&Skill> = input.skills.iter().collect();
    if candidates.len() > 254 {
        let mut scored: Vec<(f32, &Skill)> = candidates.iter().map(|s| (overlap(input.request, &skill_text(s)), *s)).collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        candidates = scored.into_iter().take(254).map(|(_, s)| s).collect();
    }
    if !candidates.is_empty() {
        questions.insert("needs_skill".into(), noul(Q_NEEDS_SKILL.into()));
        let mut criteria = Map::new();
        for s in &candidates {
            criteria.insert(s.name.clone(), json!(crate::llm::truncate(&s.description, 300)));
        }
        criteria.insert("none".into(), json!("No skill here fits the request"));
        questions.insert(
            "skill".into(),
            json!({ "type": "choice", "instructions": "Which skill's procedure fits `request` best?", "criteria": criteria }),
        );
    }
    let mut state = json!({ "request": crate::llm::truncate(input.request, 4000) });
    if !input.previous.is_empty() {
        state["previous_reply"] = json!(crate::llm::truncate(input.previous, 600));
    }
    let body = json!({ "model": cfg.jev_model, "state": state, "questions": questions });
    let resp = http
        .post(&cfg.jev_url)
        .bearer_auth(key)
        .json(&body)
        .timeout(Duration::from_millis(2500))
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "timeout".to_string() } else { e.to_string() })?;
    if !resp.status().is_success() {
        let s = resp.status();
        return Err(format!("{s}: {}", crate::llm::truncate(&resp.text().await.unwrap_or_default(), 200)));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let answers = &v["answers"];
    let p = |k: &str| answers[k]["noul"].as_f64().unwrap_or(0.0) as f32;
    let choice = |q: &str, k: &str| answers[q]["probabilities"][k].as_f64().unwrap_or(0.0) as f32;
    let t = cfg.threshold;
    let mut groups = Vec::new();
    let mut note = Vec::new();
    let mut access: Vec<(String, Group)> = vec![("web".into(), Group::Web), ("history".into(), Group::History)];
    access.extend(input.mcp.keys().map(|n| (format!("mcp:{n}"), Group::Mcp(n.clone()))));
    for (k, g) in access {
        let v = choice("access", &k);
        note.push(format!("{k}={v:.2}"));
        if v >= t {
            groups.push(g);
        }
    }
    for (k, g) in [("preference", Group::Memory), ("skill-edit", Group::SkillWrite)] {
        let v = choice("intent", k);
        note.push(format!("{k}={v:.2}"));
        if v >= t {
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
    Ok(Route {
        groups,
        skills,
        source: v["model"].as_str().unwrap_or("jev").to_string(),
        ms: 0,
        note: note.join(" "),
    })
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
    "the", "and", "for", "with", "this", "that", "you", "your", "are", "can", "how", "what", "when", "use", "from", "into", "not", "all", "any", "but", "have", "has", "was", "will", "about", "then", "them", "their", "its", "our", "out", "get", "set", "make", "please", "want", "need", "like", "just",
];

/// Share of the skill's words that appear in the request, weighted toward
/// rarer (longer) words. Crude, fast, and good enough as a fallback.
fn overlap(request: &str, doc: &str) -> f32 {
    let r: std::collections::HashSet<String> = words(request).into_iter().collect();
    let d = words(doc);
    if d.is_empty() || r.is_empty() {
        return 0.0;
    }
    let total: f32 = d.iter().map(|w| w.len() as f32).sum();
    let hit: f32 = d.iter().filter(|w| r.contains(*w)).map(|w| w.len() as f32).sum();
    hit / total
}

fn any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

fn local(cfg: &RouterConfig, input: &Input<'_>) -> Route {
    let _ = cfg;
    let t = input.request.to_lowercase();
    let mut groups = Vec::new();
    if any(&t, &["http://", "https://", "www.", "search the web", "web search", "google", "look up", "lookup", "online", "internet", "website", "docs for", "documentation", "latest version", "release notes", "news", "research"]) {
        groups.push(Group::Web);
    }
    if any(&t, &["remember", "forget", "i prefer", "i like", "i hate", "i don't like", "always ", "never ", "call me", "my name", "from now on", "stop doing", "don't ever"]) {
        groups.push(Group::Memory);
    }
    if any(&t, &["last time", "previous session", "past session", "earlier session", "we did", "yesterday", "last week", "before, we", "remember when", "chatstrata", "other session", "you said"]) {
        groups.push(Group::History);
    }
    if t.contains("skill") && any(&t, &["save", "write", "create", "update", "make", "delete", "add"]) {
        groups.push(Group::SkillWrite);
    }
    for (name, desc) in input.mcp {
        if t.contains(&name.to_lowercase()) || overlap(input.request, desc) >= 0.34 {
            groups.push(Group::Mcp(name.clone()));
        }
    }
    let mut ranked: Vec<(String, f32)> = input
        .skills
        .iter()
        .map(|s| {
            let named = t.contains(&s.name) || t.contains(&s.name.replace('-', " "));
            (s.name.clone(), if named { 1.0 } else { overlap(input.request, &skill_text(s)) })
        })
        .filter(|(_, p)| *p >= 0.25)
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(3);
    if !ranked.is_empty() || groups.contains(&Group::SkillWrite) {
        groups.push(Group::Skills);
    }
    Route { groups, skills: ranked, source: "local".into(), ms: 0, note: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_routes_obvious_cases() {
        let cfg = RouterConfig::default();
        let skills = vec![Skill {
            name: "rust-release".into(),
            description: "Cut a release of a Rust crate: bump version, changelog, tag, publish".into(),
            dir: Default::default(),
        }];
        let mcp = BTreeMap::new();
        let r = local(&cfg, &Input { request: "publish a new release of this crate with a changelog", previous: "", skills: &skills, mcp: &mcp });
        assert_eq!(r.skills.first().map(|s| s.0.as_str()), Some("rust-release"));
        let r = local(&cfg, &Input { request: "what's in https://example.com", previous: "", skills: &skills, mcp: &mcp });
        assert!(r.groups.contains(&Group::Web));
        assert!(r.skills.is_empty());
        let r = local(&cfg, &Input { request: "remember that I prefer tabs", previous: "", skills: &[], mcp: &mcp });
        assert!(r.groups.contains(&Group::Memory));
    }
}
