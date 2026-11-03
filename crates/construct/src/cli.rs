//! The command line: clap types and the subcommand bodies. `main.rs` only
//! handles the sandbox helper and calls [`run`].

mod evals;
mod headless;
mod learning;
mod providers;
mod resume;

use crate::config::{Config, Paths};
use crate::{journal, llm, memory, secrets, skills, tools, tui};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use resume::resolve_resume;
use std::io::{IsTerminal, Read, Write};

#[derive(Parser)]
#[command(name = "construct", version, about = "A lean, self-improving agent harness")]
struct Cli {
    /// Continue the latest session in this directory.
    #[arg(short = 'c', long = "continue", global = true)]
    cont: bool,
    /// Browse sessions, or resume a session by id.
    #[arg(long, global = true, num_args = 0..=1, conflicts_with = "cont")]
    resume: Option<Option<String>>,
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
    /// Store or inspect secrets (secretspec: env, then OS keyring; a Deck guest stores none).
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
    /// Store a secret in the OS keyring (value from stdin when omitted; refused in a Deck guest).
    Set { name: String, value: Option<String> },
    /// Show which known secrets resolve, and from where.
    Status,
}

/// Parse the command line and run the subcommand.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.resume == Some(None)
        && (cli.cmd.is_some() || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal())
    {
        bail!("--resume without an ID requires an interactive terminal; use --resume <id>");
    }
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
            let resume = resolve_resume(
                &cli.resume,
                cli.cont,
                &paths,
                std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            )?;
            rt.block_on(tui::run(cfg, paths, resume, cli.resume == Some(None), cli.model))
        }
        Some(Cmd::Run { prompt, json, no_learn }) => {
            let resume = resolve_resume(&cli.resume, cli.cont, &paths, false)?;
            let mut text = prompt.join(" ");
            if text == "-" || (text.is_empty() && !std::io::stdin().is_terminal()) {
                text.clear();
                std::io::stdin().read_to_string(&mut text)?;
            }
            if text.trim().is_empty() {
                bail!("nothing to run: pass a prompt");
            }
            rt.block_on(headless::run(cfg, paths, resume, text, json, !no_learn, cli.model))
        }
        Some(Cmd::Child { acp: false }) => rt.block_on(crate::child::serve::run()),
        Some(Cmd::Child { acp: true }) => {
            rt.block_on(crate::acp::server::run(cfg, paths, crate::acp::server::Mode::Child))
        }
        Some(Cmd::Acp) => rt.block_on(crate::acp::server::run(cfg, paths, crate::acp::server::Mode::User)),
        Some(Cmd::Learn { session, curate }) => rt.block_on(learning::run(cfg, paths, session, curate)),
        Some(Cmd::Secrets { cmd }) => match cmd {
            SecretsCmd::Set { name, value } => {
                // Before reading a value: a Deck guest has no credential store,
                // so the command must fail at once instead of prompting.
                if secrets::managed() {
                    bail!("{}", secrets::HOST_OWNED);
                }
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
                for status in secrets::status() {
                    println!("{}", secrets::status_line(&status));
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
            let (ep, how) = (cfg.endpoint(), cfg.discovery(&cfg.endpoint().name));
            let models = llm::catalog::refresh(&llm::Client::new(&ep, None)?, &paths, refresh, how).await?;
            println!("{}", llm::catalog::listing(&models, &query.join(" "), limit));
            Ok(())
        }),
        Some(Cmd::Providers { cmd }) => rt.block_on(providers::run(&cfg, &paths, cmd)),
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
