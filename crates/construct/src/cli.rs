//! The command line: clap types and the subcommand bodies. `main.rs` only
//! handles the sandbox helper and calls [`run`].

mod evals;
mod headless;
mod providers;

use crate::config::{Config, Paths};
use crate::{journal, learn, llm, memory, secrets, skills, tools, tui};
use anyhow::{Context, Result, bail};
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
    /// Search the endpoint's model list (OpenRouter: windows and prices).
    Models {
        /// Words the id or name must contain.
        query: Vec<String>,
        /// Fetch again even if the cached list is fresh.
        #[arg(long)]
        refresh: bool,
        /// Most rows to print.
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// List the providers, or probe one (`test <name>`).
    Providers {
        #[command(subcommand)]
        cmd: Option<providers::ProvidersCmd>,
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
    /// Serve construct as an Agent Client Protocol agent on stdin and stdout
    /// (for editors and other harnesses).
    Acp,
    /// A delegated child: reads its setup and prompts as JSON lines on stdin
    /// and writes events as JSON lines on stdout. Started by the delegate tool.
    #[command(name = "__child", hide = true)]
    Child {
        /// Speak ACP instead of JSON lines.
        #[arg(long)]
        acp: bool,
    },
}

#[derive(Subcommand)]
enum SessionsCmd {
    /// Print a session as JSONL in the old `sessions/<id>.jsonl` shape
    /// (session, message, usage, route, tool, compact rows).
    Export { id: String },
    /// Print a session as a readable transcript, found in any project's
    /// journal (messages, tool calls with ok/error, access decisions, usage).
    Show { id: String },
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
    // Best effort: a home construct can't secure still works, as before.
    if let Ok(crate::home::Outcome::Tightened { from, to }) = crate::home::make_private(&paths.home) {
        eprintln!("construct: made {} private ({from:o} -> {to:o}); see docs/adr/0021", paths.home.display());
    }
    let mut cfg = Config::load(&paths)?;
    if let Some(m) = &cli.model {
        cfg.select_model(m).context("--model")?;
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
            rt.block_on(headless::run(cfg, paths, resume, text, json, !no_learn))
        }
        Some(Cmd::Child { acp: false }) => rt.block_on(crate::child::serve::run()),
        Some(Cmd::Child { acp: true }) => {
            rt.block_on(crate::acp::server::run(cfg, paths, crate::acp::server::Mode::Child))
        }
        Some(Cmd::Acp) => rt.block_on(crate::acp::server::run(cfg, paths, crate::acp::server::Mode::User)),
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
        Some(Cmd::Sessions { cmd: SessionsCmd::Show { id } }) => {
            let (_, entries) = journal::find(&paths, &id)?;
            print!("{}", journal::transcript(&entries));
            Ok(())
        }
        Some(Cmd::Models { query, refresh, limit }) => rt.block_on(async {
            let client = llm::Client::new(&cfg.endpoint(), None)?;
            let models = llm::catalog::refresh(&client, &paths, refresh).await?;
            println!("{}", llm::catalog::listing(&models, &query.join(" "), limit));
            Ok(())
        }),
        Some(Cmd::Providers { cmd: None | Some(providers::ProvidersCmd::List) }) => {
            print!("{}", providers::list(&cfg, &paths));
            Ok(())
        }
        Some(Cmd::Providers { cmd: Some(providers::ProvidersCmd::Test { name, model }) }) => {
            rt.block_on(providers::test(&cfg, &name, model))
        }
        Some(Cmd::Memory) => {
            let store = memory::Store::new(&paths, &cfg.memory).with_project(&paths, &std::env::current_dir()?);
            let block = store.prompt_block();
            println!("{}", if block.is_empty() { "memory is empty".into() } else { block });
            Ok(())
        }
        Some(Cmd::Route) => rt.block_on(evals::route(cfg, paths)),
        Some(Cmd::Gate) => rt.block_on(evals::gate(cfg)),
        Some(Cmd::Config) => {
            println!("# {}\n{}", paths.config().display(), toml::to_string_pretty(&cfg)?);
            print!("{}", mcp_policies(&cfg, &paths, &std::env::current_dir()?));
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

/// Each MCP server's effective sandbox, as comments under the config.
fn mcp_policies(cfg: &Config, paths: &Paths, cwd: &std::path::Path) -> String {
    let mut out = String::new();
    for (name, server) in &cfg.mcp {
        let p = crate::sandbox::Policy::for_mcp(&cfg.sandbox, server, cwd, paths);
        // Building a policy makes its private temp dir; nothing runs here.
        let _ = std::fs::remove_dir(&p.tmp);
        let list = |v: &[std::path::PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        let secrets: Vec<String> = server
            .secrets
            .iter()
            .map(|s| format!("{s} ({})", if crate::secrets::get(s).is_some() { "set" } else { "not set" }))
            .collect();
        out.push_str(&format!(
            "\n# mcp.{name} runs sandboxed{}\n#   read: {}\n#   write: {}\n#   plus system paths and toolchains, read-only\n#   network: {}\n#   secrets: {}\n",
            if p.off { " (mode = \"off\": no filesystem boundary)" } else { "" },
            list(&p.read),
            list(&p.write),
            if p.no_network { "denied" } else { "allowed" },
            if secrets.is_empty() { "none".into() } else { secrets.join(", ") },
        ));
    }
    out
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
