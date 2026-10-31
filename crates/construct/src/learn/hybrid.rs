//! `[learn] strategy = "hybrid"`: one reflection per session that decides
//! each lesson's shape. A fact, convention, value or pitfall that holds for
//! any task in the repo is a playbook bullet (always in the prompt there). A
//! procedure for one kind of task, with ordered steps, is a skill (loaded
//! when needed), and gets a one-line bullet pointing at it, so being in the
//! repo is enough to know it exists: nothing has to route a request's words
//! to it. Bullets reuse `playbook` (deltas, counters, the guard); skills go
//! to the project kit's library, which checks them with the same guard.
use super::playbook::{self, Add, Books, Delta};
use super::{Outcome, ask_json};
use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use crate::skills::Library;
use anyhow::Result;
use serde::Deserialize;
use std::fmt::Write as _;

/// At most this many skills written per session: procedures are rare.
const MAX_SKILLS: usize = 2;

const SHAPE: &str = "Each lesson has a shape; pick it before writing it down.
- A rule, fact, value, convention or pitfall that applies to any task here is a playbook bullet: one instruction, one line.
- A procedure for one kind of task, with several ordered steps (cutting a release, a migration, a deploy), is a skill: `name` (lowercase-dashed, for the kind of task, e.g. \"tally-release\"), `description` (when to use it and what it isn't for, under 250 chars), `body` (markdown: numbered steps with exact commands, in order, then pitfalls actually hit). A pointer bullet to each skill is added for you; don't write one.
- To fix or extend a skill, return it again under the same name with the full new body. Don't split a procedure's steps into bullets, and don't make a skill for a single rule.
The same credential rule applies to skills: never a credential, say where it comes from.";

const REPLY: &str = "Most sessions add nothing. Reply exactly: {\"tag\": [{\"id\": \"p1\", \"tag\": \"helpful\"}], \"add\": [{\"text\": \"...\", \"scope\": \"project\"}], \"update\": [{\"id\": \"p2\", \"text\": \"...\"}], \"remove\": [\"g1\"], \"skills\": [{\"name\": \"...\", \"description\": \"...\", \"body\": \"...\"}]}";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NewSkill {
    pub name: String,
    pub description: String,
    pub body: String,
}

fn system() -> String {
    // The playbook's own rules, minus its reply format, then the shape rule.
    let rules = playbook::SYSTEM.rsplit_once("Most sessions add nothing.").map(|(r, _)| r).unwrap_or(playbook::SYSTEM);
    format!("{rules}{SHAPE}\n{REPLY}")
}

/// The pointer bullet that makes a skill visible wherever its repo is.
pub fn pointer(s: &NewSkill) -> String {
    // The first sentence says when; a following "Not for ..." would garble it.
    let d = s.description.trim();
    let what = d.split_once(". ").map(|(a, _)| a).unwrap_or(d).trim_end_matches('.');
    format!("For {}: load the `{}` skill first and follow its steps.", lower_first(what), s.name)
}

fn lower_first(s: &str) -> String {
    let s = s.strip_prefix("Use when ").or_else(|| s.strip_prefix("Use this when ")).unwrap_or(s);
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Write each skill into `lib` (create or rewrite), and add a pointer bullet
/// for each one that landed and isn't pointed at yet. Returns the log.
pub fn write_skills(lib: &Library, skills: &[NewSkill], books: &Books, d: &mut Delta) -> Vec<String> {
    let mut log = Vec::new();
    for s in skills.iter().take(MAX_SKILLS) {
        let content =
            format!("---\nname: {}\ndescription: {}\n---\n\n{}\n", s.name, s.description.trim(), s.body.trim());
        let r = match lib.get(&s.name) {
            Some(_) => lib.edit(&s.name, &content),
            None => lib.create(&s.name, &content, "agent"),
        };
        match r {
            Ok(m) => {
                log.push(format!("skill: {m}"));
                if !books.mentions(&format!("`{}`", s.name)) {
                    d.add.push(Add { text: pointer(s), scope: "project".into() });
                }
            }
            Err(e) => log.push(format!("skill {}: {e}", s.name)),
        }
    }
    log
}

pub async fn reflect(cfg: &Config, paths: &Paths, messages: &[Message]) -> Result<Outcome> {
    let ep = cfg.endpoint_for(cfg.learn_model());
    let client = llm::Client::new(&ep, ep.key())?;
    let mut books = Books::load(paths);
    let lib = books.kit.as_ref().map(|k| Library::at(k.skills_dir()));
    let mut task = format!("{}{}", playbook::session_text(messages), books.listing());
    match &lib {
        Some(lib) => {
            let _ = writeln!(task, "\nCURRENT SKILLS FOR THIS REPO:");
            let skills = lib.list();
            if skills.is_empty() {
                let _ = writeln!(task, "(none)");
            }
            for s in skills {
                let body = std::fs::read_to_string(s.dir.join("SKILL.md")).unwrap_or_default();
                let _ = writeln!(task, "### {}\n{}\n", s.name, llm::truncate(&body, 1500));
            }
        }
        None => {
            let _ = writeln!(task, "\nNot in a repo: write no skills.");
        }
    }
    let v = ask_json(&client, &cfg.wire_id(cfg.learn_model()), &system(), &task, 3000).await?;
    let mut d: Delta = serde_json::from_value(v.clone()).unwrap_or_default();
    let skills: Vec<NewSkill> = serde_json::from_value(v["skills"].clone()).unwrap_or_default();
    let mut actions = Vec::new();
    if let (Some(lib), false) = (&lib, skills.is_empty()) {
        if let Some(k) = &books.kit {
            k.touch();
        }
        actions.extend(write_skills(lib, &skills, &books, &mut d));
    }
    actions.extend(books.apply(&d));
    let summary = if actions.is_empty() { "Nothing to save.".into() } else { "Playbook and skills updated.".into() };
    Ok(Outcome { summary, actions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit(tag: &str) -> (std::path::PathBuf, Library) {
        let dir = std::env::temp_dir().join(format!("construct-hybrid-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), Library::at(dir.join("skills")))
    }

    fn release() -> NewSkill {
        NewSkill {
            name: "tally-release".into(),
            description: "Use when cutting a tally release. Not for ordinary changes.".into(),
            body: "1. Bump the version.\n2. Run ./tools/checksum after the bump.\n3. Tag it.".into(),
        }
    }

    #[test]
    fn a_procedure_becomes_a_skill_with_one_pointer_bullet() {
        let (dir, lib) = kit("pointer");
        let books = Books::empty();
        let mut d = Delta::default();
        let log = write_skills(&lib, &[release()], &books, &mut d);
        assert!(log[0].contains("created skill tally-release"), "{log:?}");
        assert!(lib.get("tally-release").is_some());
        assert_eq!(d.add.len(), 1);
        assert_eq!(
            d.add[0].text,
            "For cutting a tally release: load the `tally-release` skill first and follow its steps."
        );
        // Rewriting it later updates the skill and adds no second pointer
        // once the playbook already points at it.
        let books = Books::with_project(vec![playbook::Bullet {
            id: "p1".into(),
            text: d.add[0].text.clone(),
            ..Default::default()
        }]);
        let mut d2 = Delta::default();
        let log = write_skills(&lib, &[release()], &books, &mut d2);
        assert!(log[0].contains("rewrote"), "{log:?}");
        assert!(d2.add.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_skill_holding_a_credential_is_refused_and_not_pointed_at() {
        let (dir, lib) = kit("guard");
        let mut s = release();
        s.body.push_str("\n4. Push with GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123");
        let mut d = Delta::default();
        let log = write_skills(&lib, &[s], &Books::empty(), &mut d);
        assert!(log[0].contains("refused"), "{log:?}");
        assert!(lib.get("tally-release").is_none());
        assert!(d.add.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_prompt_keeps_the_playbook_rules_and_one_reply_format() {
        let s = system();
        assert!(s.contains("Never record a credential"));
        assert!(s.contains("is a skill"));
        assert_eq!(s.matches("Reply exactly").count(), 1);
    }
}
