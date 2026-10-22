//! The command line: clap types and the subcommand bodies. `main.rs` only
//! handles the sandbox helper and calls [`run`].

use crate::agent::{Agent, Event};
use crate::config::{Config, Paths};
use crate::{journal, learn, llm, memory, router, secrets, skills, tools, tui};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Read, Write};

#[derive(Parser)]
#[command(name = "construct", version, about = "A lean, self-improving agent harness")]
struct Cli {
    /// Continue the latest session in this directory.
    #[arg(short = 'c', long = "continue", global = true)]
    cont: bool,
    /// Resume a session by id.
    #[arg(long, global = true)]
    resume: Option<String>,
    /// Override the model for this run.
    #[arg(short, long, global = true)]
    model: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one prompt headless and print the answer ("-" reads stdin).
    Run {
        prompt: Vec<String>,
        /// Emit every event as JSON lines.
        #[arg(long)]
        json: bool,
        /// Skip the learning pass at the end.
        #[arg(long)]
        no_learn: bool,
    },
    /// Run the learning pass (normally started in the background).
    Learn {
        #[arg(long)]
        session: Option<String>,
        /// Also run the skill curator now.
        #[arg(long)]
        curate: bool,
    },
    /// Store or inspect secrets (secretspec: env, then OS keyring).
    Secrets {
        #[command(subcommand)]
        cmd: SecretsCmd,
    },
    /// List, pin or unpin skills.
    Skills {
        #[arg(value_parser = ["list", "pin", "unpin", "show"], default_value = "list")]
        action: String,
        name: Option<String>,
    },
    /// Read this project's journal.
    Sessions {
        #[command(subcommand)]
        cmd: SessionsCmd,
    },
    /// Show memory.
    Memory,
    /// Show the config path and current settings.
    Config,
    /// Print what this build and config would run as JSON: git sha, binary
    /// and config hashes, tools. Benchmarks record it with every result.
    Manifest,
    /// Route requests read as JSON lines from stdin ({"request","previous"})
    /// and print each route as a JSON line. For router evals.
    #[command(hide = true)]
    Route,
    /// Run the learning gate on JSON lines from stdin
    /// ({"users","errors","loaded"}). For gate evals.
    #[command(hide = true)]
    Gate,
}

#[derive(Subcommand)]
enum SessionsCmd {
    /// Print a session as JSONL in the old `sessions/<id>.jsonl` shape
    /// (session, message, usage, route, tool, compact rows).
    Export { id: String },
}

#[derive(Subcommand)]
enum SecretsCmd {
    /// Store a secret in the OS keyring (value from stdin when omitted).
    Set { name: String, value: Option<String> },
    /// Show which known secrets resolve.
    Status,
}

/// Parse the command line and run the subcommand.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    let mut cfg = Config::load(&paths)?;
    if let Some(m) = &cli.model {
        cfg.model = m.clone();
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(4).build()?;
    match cli.cmd {
        None => {
            let resume = resolve_resume(&cli.resume, cli.cont, &paths)?;
            rt.block_on(tui::run(cfg, paths, resume))
        }
        Some(Cmd::Run { prompt, json, no_learn }) => {
            let resume = resolve_resume(&cli.resume, cli.cont, &paths)?;
            let mut text = prompt.join(" ");
            if text == "-" || (text.is_empty() && !std::io::stdin().is_terminal()) {
                text.clear();
                std::io::stdin().read_to_string(&mut text)?;
            }
            if text.trim().is_empty() {
                bail!("nothing to run: pass a prompt");
            }
            rt.block_on(headless(cfg, paths, resume, text, json, !no_learn))
        }
        Some(Cmd::Learn { session, curate }) => rt.block_on(learn_cmd(cfg, paths, session, curate)),
        Some(Cmd::Secrets { cmd }) => match cmd {
            SecretsCmd::Set { name, value } => {
                let value = match value {
                    Some(v) => v,
                    None => {
                        if std::io::stdin().is_terminal() {
                            eprint!("{name}: ");
                        }
                        let mut s = String::new();
                        std::io::stdin().read_line(&mut s)?;
                        s.trim().to_string()
                    }
                };
                secrets::set(&name, &value)?;
                println!("stored {name} in the keyring");
                Ok(())
            }
            SecretsCmd::Status => {
                for (n, ok) in secrets::status() {
                    println!("{} {n}", if ok { "✓" } else { "·" });
                }
                Ok(())
            }
        },
        Some(Cmd::Skills { action, name }) => {
            let lib = skills::Library::new(&paths);
            match (action.as_str(), name) {
                ("list", _) => {
                    let usage = lib.usage();
                    for s in lib.list() {
                        let u = usage.get(&s.name).cloned().unwrap_or_default();
                        println!(
                            "{}{}  ({} views)  {}",
                            s.name,
                            if u.pinned { " 📌" } else { "" },
                            u.views,
                            s.description
                        );
                    }
                }
                ("show", Some(n)) => println!("{}", lib.view(&n, None).map_err(anyhow::Error::msg)?),
                ("pin", Some(n)) => lib.set_pinned(&n, true).map_err(anyhow::Error::msg)?,
                ("unpin", Some(n)) => lib.set_pinned(&n, false).map_err(anyhow::Error::msg)?,
                _ => bail!("{action} needs a skill name"),
            }
            Ok(())
        }
        Some(Cmd::Sessions { cmd: SessionsCmd::Export { id } }) => {
            let db = journal::Journal::open(&journal::path(&paths, &std::env::current_dir()?))?;
            let entries = db.since(&id, 0)?;
            if entries.is_empty() {
                bail!("no session {id} in this project's journal");
            }
            let mut out = std::io::stdout().lock();
            for line in journal::export(&entries) {
                writeln!(out, "{line}")?;
            }
            Ok(())
        }
        Some(Cmd::Memory) => {
            let store = memory::Store::new(&paths, &cfg.memory).with_project(&paths, &std::env::current_dir()?);
            let block = store.prompt_block();
            println!("{}", if block.is_empty() { "memory is empty".into() } else { block });
            Ok(())
        }
        Some(Cmd::Route) => rt.block_on(route_cmd(cfg, paths)),
        Some(Cmd::Gate) => rt.block_on(gate_cmd(cfg)),
        Some(Cmd::Config) => {
            println!("# {}\n{}", paths.config().display(), toml::to_string_pretty(&cfg)?);
            Ok(())
        }
        Some(Cmd::Manifest) => {
            let catalog = tools::Catalog::build(&cfg.tools, None)?;
            let m = crate::manifest::Manifest::new(&cfg, &catalog).with_binary_hash();
            println!("{}", serde_json::to_string_pretty(&m)?);
            Ok(())
        }
    }
}

fn stdin_lines() -> Vec<serde_json::Value> {
    let mut text = String::new();
    let _ = std::io::stdin().read_to_string(&mut text);
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

async fn route_cmd(cfg: Config, paths: Paths) -> Result<()> {
    let skills = skills::Library::new(&paths).list();
    let mcp: std::collections::BTreeMap<String, String> =
        cfg.mcp.iter().map(|(k, v)| (k.clone(), v.description.clone())).collect();
    let http = reqwest::Client::builder().tcp_nodelay(true).build()?;
    for v in stdin_lines() {
        let request = v["request"].as_str().unwrap_or("");
        let previous = v["previous"].as_str().unwrap_or("");
        let r =
            router::route(&cfg.router, &http, router::Input { request, previous, skills: &skills, mcp: &mcp }).await;
        let out = serde_json::json!({
            "request": request,
            "groups": r.groups.iter().map(tools::Group::label).collect::<Vec<_>>(),
            "skills": r.skills,
            "source": r.source,
            "ms": r.ms,
            "note": r.note,
        });
        println!("{out}");
    }
    Ok(())
}

async fn gate_cmd(cfg: Config) -> Result<()> {
    let strs = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
    };
    for v in stdin_lines() {
        let started = std::time::Instant::now();
        let g = learn::gate_parts(&cfg, &strs(&v["users"]), &strs(&v["errors"]), &strs(&v["loaded"])).await;
        let out = serde_json::json!({ "kind": g.kind.map(|k| format!("{k:?}")), "source": g.source, "note": g.note, "ms": started.elapsed().as_millis() as u64 });
        println!("{out}");
    }
    Ok(())
}

fn resolve_resume(resume: &Option<String>, cont: bool, paths: &Paths) -> Result<Option<String>> {
    if let Some(id) = resume {
        return Ok(Some(id.clone()));
    }
    if cont {
        let cwd = std::env::current_dir()?;
        let db = journal::Journal::open(&journal::path(paths, &cwd))?;
        return db.latest(&cwd.display().to_string());
    }
    Ok(None)
}

async fn headless(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    text: String,
    json: bool,
    learn: bool,
) -> Result<()> {
    let learn = learn && cfg.learn.enabled;
    let cwd = std::env::current_dir()?;
    let mut agent = Agent::new(cfg, paths, cwd, resume)?;
    // Overlaps the model's TLS handshake with routing.
    agent.warm();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let printer = tokio::spawn(async move {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut failed = false;
        let mut at_line_start = true;
        while let Some(ev) = rx.recv().await {
            if json {
                let _ = writeln!(out, "{}", serde_json::to_string(&ev).unwrap_or_default());
                let _ = out.flush();
                if matches!(ev, Event::Error { .. }) {
                    failed = true;
                }
                continue;
            }
            match ev {
                Event::Text { text } => {
                    let _ = write!(out, "{text}");
                    let _ = out.flush();
                    at_line_start = text.ends_with('\n');
                }
                Event::ToolStart { name, args, .. } => {
                    if !at_line_start {
                        let _ = writeln!(out);
                        at_line_start = true;
                    }
                    let _ = writeln!(err, "· {name} {}", llm::truncate(&args.replace('\n', " "), 160));
                }
                Event::ToolEnd { ok: false, output, .. } => {
                    let _ = writeln!(err, "  ✗ {}", llm::truncate(&output.replace('\n', " "), 200));
                }
                Event::Error { message } => {
                    failed = true;
                    let _ = writeln!(err, "error: {message}");
                }
                Event::TurnEnd { .. } if !at_line_start => {
                    let _ = writeln!(out);
                }
                _ => {}
            }
        }
        failed
    });
    agent.turn(&text, &tx).await?;
    drop(tx);
    let failed = printer.await.unwrap_or(true);
    if learn && learn::kind_for(agent.user_turns, agent.tool_calls).is_some() {
        learn::spawn_detached(&agent.journal.session);
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

async fn learn_cmd(cfg: Config, paths: Paths, session: Option<String>, curate: bool) -> Result<()> {
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
            // A cheap check first: most sessions hold nothing worth a review.
            let gate = learn::gate(&cfg, &fresh).await;
            learn::log_line(&paths, &format!("session {id}: gate {} {} -> {:?}", gate.source, gate.note, gate.kind));
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
