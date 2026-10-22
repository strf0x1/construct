use super::gate::skills_loaded;
use super::ledger::{append_ledger, ledger, observe, recurrences};
use super::prompts::{MEMORY_FOCUS, NOTHING, SKILL_FOCUS, SYSTEM};
use super::{Kind, Outcome, digest, learner, run_loop};
use crate::config::{Config, Paths};
use crate::llm::{self, Message};
use crate::memory::Store;
use crate::tools::Group;
use anyhow::Result;
use std::fmt::Write as _;
use std::sync::Arc;

/// Run one review over `messages`. Returns what changed.
///
/// Skills are written from recurrence, not from one session: what this
/// session taught goes into the observation ledger first, and a new skill is
/// only written when the same problem class has come up in another session
/// too. A skill that was loaded and turned out wrong can be patched at once.
pub async fn review(cfg: &Config, paths: &Paths, session: &str, messages: &[Message], kind: Kind) -> Result<Outcome> {
    let key = crate::secrets::api_key(&cfg.api_key_secret);
    let client = llm::Client::new(cfg, key)?;
    let memory = Store::new(paths, &cfg.memory);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let (ctx, catalog) = learner(cfg, paths, memory, rx)?;
    let ctx = Arc::new(ctx);
    let wants_memory = matches!(kind, Kind::Memory | Kind::Both);
    let mut ledger_note = Vec::new();

    let loaded = skills_loaded(messages);
    // Observe whenever the gate fired: a procedure the user had to supply
    // reads as a preference to the gate, but it is exactly what recurs.
    let recurring = {
        let prior = ledger(paths);
        let new = observe(&client, cfg.learn_model(), messages, session).await.unwrap_or_default();
        append_ledger(paths, &new);
        for o in &new {
            ledger_note.push(format!("ledger: {}", o.problem_class));
        }
        recurrences(cfg, &new, &prior).await
    };
    let skills_ok = !loaded.is_empty() || !recurring.is_empty();
    if !wants_memory && !skills_ok {
        let summary = if ledger_note.is_empty() {
            "Nothing to save.".to_string()
        } else {
            format!("Recorded {} observation(s); a skill is written if they recur.", ledger_note.len())
        };
        return Ok(Outcome { summary, actions: ledger_note });
    }

    let lib = ctx.skills.list();
    let mut task = String::new();
    let _ = writeln!(task, "<session>\n{}\n</session>\n", digest(messages, 60_000));
    let _ = writeln!(
        task,
        "Current memory:{}",
        if ctx.memory.prompt_block().is_empty() { " (empty)".to_string() } else { ctx.memory.prompt_block() }
    );
    let project_lib = ctx.project_skills.as_ref().map(|p| p.list()).unwrap_or_default();
    if lib.is_empty() && project_lib.is_empty() {
        let _ = writeln!(task, "\nSkill library: empty.");
    } else {
        let _ = writeln!(task, "\nSkill library:");
        for k in &project_lib {
            let _ = writeln!(task, "- {} (this project): {}", k.name, k.description);
        }
        for s in &lib {
            let _ = writeln!(task, "- {}: {}", s.name, s.description);
        }
    }
    let _ = writeln!(task);
    if wants_memory {
        let _ = writeln!(task, "{MEMORY_FOCUS}\n");
    }
    if skills_ok {
        let _ = writeln!(task, "{SKILL_FOCUS}\n");
        if recurring.is_empty() {
            let _ = writeln!(
                task,
                "Only patch the skills loaded this session ({}); don't create new skills. Anything else this session taught is already recorded and will become a skill if it recurs.\n",
                loaded.join(", ")
            );
        } else {
            let _ = writeln!(
                task,
                "RECURRING PROBLEMS: these came up in more than one session, so they have earned a skill. Write or patch one class-level skill per problem class (fold it into an existing skill when one covers the territory). Use scope 'project' when every occurrence is in the same repo and the fix only makes sense there; 'global' when it is a method that carries across repos. When patching, add only what this evidence shows, never advice about today's task."
            );
            for (o, earlier) in &recurring {
                for e in std::iter::once(o).chain(earlier) {
                    let _ = writeln!(
                        task,
                        "- [session {} · repo {}] {}: {} | fix: {} | evidence: {}",
                        e.session,
                        e.cwd,
                        e.problem_class,
                        e.summary,
                        e.fix,
                        llm::truncate(&e.evidence, 300)
                    );
                }
            }
            if !loaded.is_empty() {
                let _ = writeln!(task, "You may also patch the skills loaded this session ({}).", loaded.join(", "));
            }
            let _ = writeln!(task, "Don't create skills for anything else.\n");
        }
    }
    let _ = writeln!(task, "{NOTHING}");

    let mut groups = vec![];
    if wants_memory {
        groups.push(Group::Memory);
    }
    if skills_ok {
        groups.extend([Group::Skills, Group::SkillWrite]);
    }
    let defs = catalog.definitions(&groups);
    let mut out = run_loop(
        &client,
        cfg.learn_model(),
        &ctx,
        &catalog,
        vec![Message::system(SYSTEM), Message::user(task)],
        &defs,
        10,
    )
    .await?;
    ledger_note.append(&mut out.actions);
    out.actions = ledger_note;
    Ok(out)
}
