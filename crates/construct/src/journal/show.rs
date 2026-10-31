//! Reading a session for people: the `/status` block, the `sessions show`
//! transcript, and finding a session by id in any project's journal. Every
//! number here is summed from the journal's own rows, so what the TUI prints
//! is what a coding agent reading the file would add up. The one design
//! choice: nothing is kept in memory by the UI for this; the journal is asked.

use super::{Entry, Journal, Totals};
use crate::config::{Config, Paths};
use crate::event::Event;
use crate::llm::truncate;
use anyhow::{Result, bail};
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// A count with a unit suffix: `968k`, `1.20M`.
pub fn human(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..100_000 => format!("{:.1}k", n as f64 / 1e3),
        100_000..1_000_000 => format!("{}k", n / 1000),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}

/// `~`-shortened path for display.
fn tilde(p: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from).and_then(|h| p.strip_prefix(h).ok().map(|r| r.to_path_buf())) {
        Some(r) => format!("~/{}", r.display()),
        None => p.display().to_string(),
    }
}

/// The `/status` rows: a label and its value.
pub fn status_rows(session: &str, journal: &Path, cfg: &Config, t: &Totals) -> Vec<(&'static str, String)> {
    let (u, window) = (&t.usage, cfg.context_window);
    let used = share(t.last_prompt * 100 / window.max(1) as u64, t.last_prompt, cfg.on_router());
    let mut rows = vec![
        ("session", session.to_string()),
        ("journal", tilde(journal)),
        ("model", cfg.qualified_model()),
        ("endpoint", cfg.endpoint_label()),
        ("context", format!("{} ({used} used) · from {}", human(window as u64), cfg.window_source())),
        ("requests", t.requests.to_string()),
        (
            "input",
            format!("{} (cached {}, {:.0}%)", human(u.prompt_tokens), human(u.cached_tokens), u.hit_rate() * 100.0),
        ),
        ("output", human(u.completion_tokens)),
        ("cost", u.cost.map_or("n/a".to_string(), |c| format!("${c:.3}"))),
    ];
    let side = &t.side;
    if side.prompt_tokens + side.completion_tokens > 0 {
        let cost = side.cost.map_or(String::new(), |c| format!(", ${c:.3}"));
        rows.push((
            "side calls",
            format!(
                "input {}, output {}{cost} (summaries, sub-agents; in the totals above)",
                human(side.prompt_tokens),
                human(side.completion_tokens)
            ),
        ));
    }
    rows
}

/// `rows` as text, one `label  value` line each, the values lined up.
pub fn rows_text(rows: &[(&str, String)]) -> String {
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    rows.iter().map(|(k, v)| format!("{k:<w$}  {v}")).collect::<Vec<_>>().join("\n")
}

/// How full the window is, as people read it: `37%`, `<1%` once anything
/// was sent, and `~37%` for a router, whose window is the router's rather
/// than that of whichever model answered.
pub fn share(pct: u64, last_prompt: u64, router: bool) -> String {
    match (pct, router) {
        (0, _) if last_prompt > 0 => "<1%".into(),
        (p, true) => format!("~{p}%"),
        (p, false) => format!("{p}%"),
    }
}

/// Where a session lives: the first project journal under `paths.home`
/// holding rows for `id`.
pub fn find(paths: &Paths, id: &str) -> Result<(PathBuf, Vec<Entry>)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(paths.home.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|d| d.path().join("journal.sqlite"))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    for f in files {
        let Ok(db) = Journal::open(&f) else { continue };
        let entries = db.since(id, 0)?;
        if !entries.is_empty() {
            return Ok((f, entries));
        }
    }
    bail!("no session {id} in any project under {}", paths.home.display())
}

/// A session as readable text: messages, tool calls with their outcome,
/// access decisions and usage.
pub fn transcript(entries: &[Entry]) -> String {
    let mut out = String::new();
    for Entry { event, .. } in entries {
        match event {
            Event::Head { id, cwd, model, version, parent_session, .. } => {
                let _ = writeln!(out, "# session {id}\n# cwd {cwd}\n# model {model} · construct {version}");
                if let Some(p) = parent_session {
                    let _ = writeln!(out, "# child of {p}");
                }
                out.push('\n');
            }
            Event::User { message } => {
                let text = message.content.as_deref().unwrap_or("");
                let text = text.split("\n\n<system-reminder>").next().unwrap_or("");
                let _ = writeln!(out, "## user\n{text}\n");
            }
            Event::Assistant { message } => {
                let text = message.content.as_deref().unwrap_or("").trim();
                if !text.is_empty() {
                    let _ = writeln!(out, "## assistant\n{text}\n");
                }
            }
            Event::ToolStart { name, args, .. } => {
                let _ = writeln!(out, "-> {name} {}", truncate(args, 300));
            }
            Event::ToolEnd { name, ok, output, ms, code, .. } => {
                let status = if *ok { "ok".to_string() } else { format!("error {}", code.as_deref().unwrap_or("")) };
                let _ = writeln!(out, "<- {name} {} ({ms}ms)", status.trim_end());
                if !*ok || !output.is_empty() {
                    let _ = writeln!(out, "{}\n", indent(&truncate(output, 600)));
                }
            }
            Event::Access { tool, access, paths, decision, agent } => {
                let who = if agent.is_empty() { String::new() } else { format!(" [{agent}]") };
                let _ = writeln!(out, "!! access {decision}: {tool}{who} wants {access} {}\n", paths.join(", "));
            }
            Event::Permission { title, decision, scope, .. } => {
                let _ = writeln!(out, "!! permission {decision} ({scope}): {title}\n");
            }
            Event::Usage { prompt, completion, cached, cost, .. } => {
                let cost = cost.map_or(String::new(), |c| format!(", ${c:.3}"));
                let _ = writeln!(out, "   usage in {prompt} (cached {cached}) out {completion}{cost}");
            }
            Event::Compacted { mode, tokens_before, tokens_after, .. } => {
                let _ = writeln!(out, "   compacted ({mode}) ~{tokens_before} -> ~{tokens_after} tokens\n");
            }
            Event::Error { message } => {
                let _ = writeln!(out, "!! error: {message}\n");
            }
            Event::Note { class, kind, detail, count } => {
                let _ = writeln!(out, "   note {class}/{kind} x{count}: {detail}");
            }
            _ => {}
        }
    }
    let t = Totals::of(entries);
    let u = &t.usage;
    let _ = write!(
        out,
        "# totals: {} requests, in {} (cached {}), out {}",
        t.requests, u.prompt_tokens, u.cached_tokens, u.completion_tokens
    );
    if let Some(c) = u.cost {
        let _ = write!(out, ", ${c:.3}");
    }
    out.push('\n');
    out
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("   {l}")).collect::<Vec<_>>().join("\n")
}
