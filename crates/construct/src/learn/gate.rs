use super::ledger::{Observation, ledger};
use super::{Kind, log_line_quiet};
use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use serde_json::{Value, json};

/// The verdict of the cheap check that runs before an LLM review.
pub struct Gate {
    pub kind: Option<Kind>,
    pub source: &'static str,
    pub note: String,
}

fn user_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| m.content.as_deref())
        .map(|c| c.split("<system-reminder>").next().unwrap_or("").trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// Tool results that report a failure, with the call that produced them.
fn tool_errors(messages: &[Message]) -> Vec<String> {
    let mut calls = std::collections::HashMap::new();
    for m in messages {
        for c in &m.tool_calls {
            calls.insert(c.id.clone(), format!("{} {}", c.function.name, llm::truncate(&c.function.arguments, 160)));
        }
    }
    messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| {
            let out = m.content.as_deref()?;
            out.starts_with("Error:").then(|| {
                let call = m.tool_call_id.as_ref().and_then(|id| calls.get(id)).cloned().unwrap_or_default();
                format!("{call} -> {}", llm::truncate(out, 240))
            })
        })
        .collect()
}

pub(super) fn skills_loaded(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|m| &m.tool_calls)
        .filter(|c| c.function.name == "skill")
        .filter_map(|c| serde_json::from_str::<Value>(&c.function.arguments).ok()?["name"].as_str().map(String::from))
        .collect()
}

const PREFERENCE_HINTS: &[&str] = &[
    "i prefer",
    "i like",
    "i don't like",
    "i hate",
    "i want you to",
    "always ",
    "never ",
    "stop ",
    "don't ",
    "do not ",
    "remember",
    "from now on",
    "call me",
    "my name",
    "too verbose",
    "too long",
    "next time",
    "on my machine",
];

/// Decide whether a stretch of session holds anything worth an LLM review,
/// before paying for one. Jev answers when a TypeSafe key resolves; local
/// heuristics otherwise.
pub async fn gate(cfg: &Config, messages: &[Message]) -> Gate {
    let users = user_texts(messages);
    if users.is_empty() {
        return Gate { kind: None, source: "local", note: "no user turns".into() };
    }
    let errors = tool_errors(messages);
    let loaded = skills_loaded(messages);
    let g = gate_parts(cfg, &users, &errors, &loaded).await;
    if g.kind.is_some() {
        return g;
    }
    // The user telling us something we've already been told is the strongest
    // sign of recurrence, even when it reads as an ordinary request.
    match repeats_ledger(cfg, &users).await {
        Some(lesson) => {
            Gate { kind: Some(Kind::Both), source: g.source, note: format!("{} repeats_ledger={lesson:?}", g.note) }
        }
        None => g,
    }
}

/// The ledger entry the user is repeating in `users`, if any.
async fn repeats_ledger(cfg: &Config, users: &[String]) -> Option<String> {
    let paths = Paths::resolve().ok()?;
    let ledger = ledger(&paths);
    let recent: Vec<&Observation> = ledger.iter().rev().take(60).collect();
    if recent.is_empty() {
        return None;
    }
    let lesson = |o: &Observation| {
        format!("{}: {} (fix: {})", o.problem_class, llm::truncate(&o.summary, 160), llm::truncate(&o.fix, 160))
    };
    if let Some(key) =
        crate::secrets::get("TYPESAFE_API_KEY").filter(|_| cfg.router.mode != "local" && cfg.router.mode != "off")
    {
        let mut criteria = serde_json::Map::new();
        // "none" first: Jev favours the first option (see src/router.rs).
        criteria.insert("none".into(), json!("the user doesn't restate any of these"));
        for (i, o) in recent.iter().enumerate() {
            criteria.insert(format!("o{i}"), json!(lesson(o)));
        }
        let mut q = serde_json::Map::new();
        q.insert("repeat".into(), json!({ "type": "choice", "instructions": "Does the user, in `user_messages`, tell the assistant one of these earlier lessons again?", "criteria": criteria }));
        let users: Vec<String> = users.iter().rev().take(12).rev().map(|u| llm::truncate(u, 500)).collect();
        if let Ok(v) = crate::jev::ask(
            &cfg.router,
            &reqwest::Client::new(),
            &key,
            "repeat",
            json!({ "user_messages": users }),
            q,
            std::time::Duration::from_secs(5),
        )
        .await
        {
            let probs = v["answers"]["repeat"]["probabilities"].as_object().cloned().unwrap_or_default();
            return probs
                .iter()
                .filter(|(k, p)| *k != "none" && p.as_f64().unwrap_or(0.0) >= 0.5)
                .filter_map(|(k, _)| k[1..].parse::<usize>().ok())
                .next()
                .map(|i| recent[i].problem_class.clone());
        }
    }
    recent
        .iter()
        .find(|o| users.iter().any(|u| crate::router::overlap(u, &format!("{} {}", o.fix, o.summary)) >= 0.4))
        .map(|o| o.problem_class.clone())
}

/// The gate on already-extracted evidence; `construct gate` feeds it directly.
pub async fn gate_parts(cfg: &Config, users: &[String], errors: &[String], loaded: &[String]) -> Gate {
    if users.is_empty() {
        return Gate { kind: None, source: "local", note: "no user turns".into() };
    }
    if cfg.router.mode != "local"
        && cfg.router.mode != "off"
        && let Some(key) = crate::secrets::get("TYPESAFE_API_KEY")
    {
        match jev_gate(cfg, &key, users, errors, loaded).await {
            Ok(g) => return g,
            Err(e) => log_line_quiet(&format!("jev gate failed ({e}); using local heuristics")),
        }
    }
    let joined = users.join("\n").to_lowercase();
    let pref = PREFERENCE_HINTS.iter().any(|h| joined.contains(h));
    let skill = !loaded.is_empty() || errors.len() >= 3;
    let kind = match (pref, skill) {
        (true, true) => Some(Kind::Both),
        (true, false) => Some(Kind::Memory),
        (false, true) => Some(Kind::Skills),
        _ => None,
    };
    Gate {
        kind,
        source: "local",
        note: format!("preference={pref} tool_errors={} skills_loaded={}", errors.len(), loaded.len()),
    }
}

async fn jev_gate(
    cfg: &Config,
    key: &str,
    users: &[String],
    errors: &[String],
    loaded: &[String],
) -> Result<Gate, String> {
    let users: Vec<String> = users.iter().rev().take(12).rev().map(|u| llm::truncate(u, 500)).collect();
    let errors: Vec<String> = errors.iter().take(8).cloned().collect();
    let noul = |q: &str| json!({ "type": "noul", "instructions": q });
    // Each question sees only the state it needs: whether the user stated a
    // preference doesn't depend on tool errors, and long error text pulled
    // that answer around. So up to two calls, in parallel.
    let mut pref_q = serde_json::Map::new();
    // A choice, not a noul: asked as yes/no, Jev counted task phrasing ("I want
    // the parser to...", "make sure it always...") as preferences. "task" goes
    // first: Jev favours the first option (see src/router.rs).
    pref_q.insert(
        "preference".into(),
        json!({ "type": "choice", "instructions": "Across `user_messages`, what is the most lasting thing the user said?", "criteria": {
            "task": "only requests for this task, or corrections that apply to this task alone",
            "preference": "a rule for how the assistant should work in future sessions, or a durable fact about the user or their machine",
        } }),
    );
    let mut skill_q = serde_json::Map::new();
    if !errors.is_empty() {
        skill_q.insert(
            "workaround".into(),
            noul("Do `tool_errors` show the assistant's own commands or approach failing in a way that needed a different technique (wrong flags, missing tools, environment quirks), as opposed to failures in the user's code that the task was about?"),
        );
    }
    if !loaded.is_empty() {
        skill_q.insert(
            "skill_gap".into(),
            noul("Do `user_messages` or `tool_errors` suggest that a procedure listed in `skills_loaded` was wrong, incomplete or outdated?"),
        );
    }
    let http = reqwest::Client::new();
    let timeout = std::time::Duration::from_secs(5);
    let pref_call =
        crate::jev::ask(&cfg.router, &http, key, "gate", json!({ "user_messages": users }), pref_q, timeout);
    let skill_call = async {
        if skill_q.is_empty() {
            return Ok(json!({}));
        }
        let state = json!({ "user_messages": users, "tool_errors": errors, "skills_loaded": loaded });
        crate::jev::ask(&cfg.router, &http, key, "gate", state, skill_q, timeout).await
    };
    let (pv, sv) = tokio::join!(pref_call, skill_call);
    let (pv, sv) = (pv?, sv?);
    let v = json!({ "answers": {
        "preference": pv["answers"]["preference"],
        "workaround": sv["answers"]["workaround"],
        "skill_gap": sv["answers"]["skill_gap"],
    } });
    let p = |k: &str| v["answers"][k]["noul"].as_f64().unwrap_or(0.0);
    let pref_p = v["answers"]["preference"]["probabilities"]["preference"].as_f64().unwrap_or(0.0);
    let pref = pref_p >= 0.5;
    let skill = p("workaround") >= 0.5 || p("skill_gap") >= 0.5;
    let kind = match (pref, skill) {
        (true, true) => Some(Kind::Both),
        (true, false) => Some(Kind::Memory),
        (false, true) => Some(Kind::Skills),
        _ => None,
    };
    Ok(Gate {
        kind,
        source: "jev",
        note: format!("preference={pref_p:.2} workaround={:.2} skill_gap={:.2}", p("workaround"), p("skill_gap")),
    })
}
