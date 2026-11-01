//! `construct providers`: what each provider is, and whether it answers.
//!
//! The design choice: `list` reads only config, secrets and the cached model
//! lists, so it is instant and offline; `test` and `add` are the places
//! construct probes a provider on purpose (`test`: its models, a short reply,
//! a tool call, each step timed on its own line, so a misbehaving local
//! server shows which part fails). `add` writes one table into the user's
//! `config.toml` in place and never a key: it names the command that stores
//! one, since typing it here would echo it.

use crate::config::{Config, Paths, write_atomic};
use crate::llm::{self, Message, catalog};
use crate::providers::{Discover, Provider};
use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::Subcommand;
use serde_json::json;
use std::time::{Duration, Instant};

#[derive(Subcommand)]
pub(super) enum ProvidersCmd {
    /// Each provider: url, dialect, key, catalogued and cached models.
    List,
    /// Add `[providers.<name>]`: probe the server, guess how to discover its
    /// models, and write the table into config.toml.
    Add {
        name: String,
        /// The server, up to and including `/v1`.
        #[arg(long)]
        url: String,
        /// The secret holding its key. Default: none for a loopback url,
        /// else `<NAME>_API_KEY`.
        #[arg(long)]
        key_secret: Option<String>,
        /// `openai`, `vllm`, `llama.cpp`, `ollama` or `off`. Default: guessed
        /// from what the server answers to.
        #[arg(long)]
        discover: Option<String>,
    },
    /// Probe one provider: its model list, a short reply and a tool call.
    Test {
        name: String,
        /// The id to try, as the provider knows it. Default: its first
        /// catalogued model, else the first it lists.
        #[arg(long)]
        model: Option<String>,
    },
}

/// `construct providers [list | add | test]`.
pub(super) async fn run(cfg: &Config, paths: &Paths, cmd: Option<ProvidersCmd>) -> Result<()> {
    match cmd {
        None | Some(ProvidersCmd::List) => {
            print!("{}", list(cfg, paths));
            // The cached lists too: a name can hide ids only a list has.
            let mut cfg = cfg.clone();
            llm::overlay::overlay(&mut cfg, paths);
            for w in cfg.shadowed() {
                println!("warning: {w}");
            }
            Ok(())
        }
        Some(ProvidersCmd::Add { name, url, key_secret, discover }) => {
            add(cfg, paths, &name, &url, key_secret, discover).await
        }
        Some(ProvidersCmd::Test { name, model }) => test(cfg, paths, &name, model).await,
    }
}

fn list(cfg: &Config, paths: &Paths) -> String {
    let mut rows = vec![["provider", "url", "dialect", "key", "models"].map(String::from)];
    for name in cfg.provider_names() {
        let Some((ep, ids)) = cfg.provider(&name) else { continue };
        let key = match (ep.api_key_secret.as_str(), ep.key().is_some()) {
            ("", _) => "none".to_string(),
            (s, true) => format!("{s} (set)"),
            (s, false) => format!("{s} (not set)"),
        };
        let cached = catalog::cached(paths, &ep.base_url).len();
        let mut listed = match catalog::cache_age(paths, &ep.base_url) {
            Some(age) => format!("{} catalogued · {cached} listed {} ago", ids.len(), ago(age)),
            None => format!("{} catalogued · list not fetched", ids.len()),
        };
        if let Some((at, why)) = catalog::reach(paths, &ep.base_url).failed {
            let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            listed
                .push_str(&format!(" · unreachable {} ago: {why}", ago(since.saturating_sub(Duration::from_secs(at)))));
        }
        let dialect = llm::Provider::of(&ep).name().to_string();
        rows.push([name, ep.base_url, dialect, key, listed]);
    }
    let widths: Vec<usize> = (0..5).map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for r in rows {
        let line: Vec<String> = r.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// `construct providers add`: refuses a taken name, a bad table and a
/// server that doesn't answer (unless `--discover off`), and leaves the file
/// as it was when the result would not load.
async fn add(
    cfg: &Config,
    paths: &Paths,
    name: &str,
    url: &str,
    key_secret: Option<String>,
    discover: Option<String>,
) -> Result<()> {
    let file = paths.config();
    ensure!(
        !cfg.provider_names().iter().any(|n| n == name),
        "{name} is already a provider; edit it in {}",
        file.display()
    );
    let mut decl = Provider {
        base_url: url.trim_end_matches('/').to_string(),
        api_key_secret: key_secret.clone(),
        ..Provider::default()
    };
    decl.check(name)?;
    let ep = decl.endpoint(name);
    let key = if ep.api_key_secret.is_empty() { None } else { ep.key() };
    let client = llm::Client::new(&ep, key.clone())?;
    let how = match discover {
        Some(d) => serde_json::from_value(json!(d))
            .map_err(|_| anyhow!("--discover {d:?}: one of openai, vllm, llama.cpp, ollama, off"))?,
        None => llm::discover::guess(&client).await,
    };
    let listed = match how {
        Discover::Off => vec![],
        _ => llm::discover::fetch(&client, how).await.map_err(|e| match key {
            None if !ep.api_key_secret.is_empty() => {
                anyhow!(
                    "{name}: {e:#}; it may want its key: construct secrets set {}, then add it again",
                    ep.api_key_secret
                )
            }
            _ => anyhow!(
                "{name}: {e:#}; check --url (up to and including /v1), or pass --discover off to add it unprobed"
            ),
        })?,
    };
    decl.discover = how;
    let before = std::fs::read_to_string(&file).unwrap_or_default();
    write_atomic(&file, with_table(&before, name, &decl)?.as_bytes())?;
    if let Err(e) = Config::load(paths) {
        write_atomic(&file, before.as_bytes())?;
        return Err(e.context(format!("{} would not load with [providers.{name}]; left as it was", file.display())));
    }
    if !listed.is_empty() {
        catalog::store(paths, &ep.base_url, &listed);
    }
    let window =
        listed.iter().filter_map(|m| m.context).max().map_or(String::new(), |w| format!(", windows up to {w}"));
    let key_line = match (ep.api_key_secret.as_str(), key.is_some()) {
        ("", _) => "none".to_string(),
        (s, true) => s.to_string(),
        (s, false) => format!("{s}, not set yet: construct secrets set {s}"),
    };
    println!("added [providers.{name}] to {}", file.display());
    println!("  url       {}", ep.base_url);
    println!("  key       {key_line}");
    println!("  discover  {} ({} listed{window})", json!(how).as_str().unwrap_or_default(), listed.len());
    println!("next: construct providers test {name}, then /model {name}/<id>");
    Ok(())
}

/// `text` with `[providers.<name>]` added, everything else as it was.
fn with_table(text: &str, name: &str, p: &Provider) -> Result<String> {
    use toml_edit::{DocumentMut, Item, Table, value};
    let mut doc: DocumentMut = text.parse().context("parsing config.toml")?;
    let providers = doc.entry("providers").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let Some(providers) = providers.as_table_mut() else { bail!("config.toml: `providers` is not a table") };
    let mut t = Table::new();
    t["base_url"] = value(&p.base_url);
    if let Some(s) = &p.api_key_secret {
        t["api_key_secret"] = value(s);
    }
    if p.discover != Discover::Openai {
        t["discover"] = value(json!(p.discover).as_str().unwrap_or_default());
    }
    providers.insert(name, Item::Table(t));
    Ok(doc.to_string())
}

fn ago(d: Duration) -> String {
    match d.as_secs() {
        s @ 0..60 => format!("{s}s"),
        s @ 60..3600 => format!("{}m", s / 60),
        s @ 3600..86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// Probe `name`: prints one line per step and fails if any step did.
async fn test(cfg: &Config, paths: &Paths, name: &str, model: Option<String>) -> Result<()> {
    let Some((ep, ids)) = cfg.provider(name) else {
        bail!("no provider {name:?}; there are: {}", cfg.provider_names().join(", "));
    };
    let dialect = llm::Provider::of(&ep).name();
    let key_name = if ep.api_key_secret.is_empty() { "no key".to_string() } else { ep.api_key_secret.clone() };
    println!("{name}  {}  {dialect}  {key_name}", ep.base_url);
    let client = llm::Client::new(&ep, ep.require(ep.key())?)?;
    let mut failed = 0;

    let (started, how) = (Instant::now(), cfg.discovery(name));
    let listed = match llm::discover::fetch(&client, how).await {
        Ok(m) => {
            println!("models  ok      {} listed in {} ms ({how:?})", m.len(), started.elapsed().as_millis());
            catalog::store(paths, &ep.base_url, &m);
            m
        }
        Err(e) => {
            failed += 1;
            println!("models  FAILED  {e:#}");
            vec![]
        }
    };
    let Some(model) = model.or_else(|| ids.first().cloned()).or_else(|| listed.first().map(|m| m.id.clone())) else {
        bail!("{name}: no model to try (none catalogued or listed); pass --model");
    };

    let started = Instant::now();
    let msgs = [Message::user("Reply with exactly: ok")];
    let req = llm::Request { model: &model, messages: &msgs, tools: &[], max_tokens: Some(512), temperature: None };
    match client.complete(req).await {
        Ok(c) => {
            let text = c.message.content.unwrap_or_default();
            let (u, ms) = (c.usage, started.elapsed().as_millis());
            let said = llm::truncate(text.trim(), 40);
            println!(
                "reply   ok      {said:?} from {model} in {ms} ms, {} in / {} out",
                u.prompt_tokens, u.completion_tokens
            );
        }
        Err(e) => {
            failed += 1;
            println!("reply   FAILED  {model}: {e:#}");
        }
    }

    let started = Instant::now();
    let ping = json!({ "type": "function", "function": {
        "name": "ping", "description": "Answer a connectivity check.",
        "parameters": { "type": "object", "properties": {} } } });
    let msgs = [Message::user("Call the ping tool now.")];
    let tools = [ping];
    let req = llm::Request { model: &model, messages: &msgs, tools: &tools, max_tokens: Some(512), temperature: None };
    match client.complete(req).await {
        Ok(c) if c.message.tool_calls.iter().any(|t| t.function.name == "ping") => {
            println!("tools   ok      called ping in {} ms", started.elapsed().as_millis());
        }
        Ok(c) => {
            failed += 1;
            println!("tools   FAILED  no tool call (finish {:?}); this model may not do tools here", c.finish_reason);
        }
        Err(e) => {
            failed += 1;
            println!("tools   FAILED  {e:#}");
        }
    }

    match catalog::find(&listed, &model).and_then(|m| m.context) {
        Some(w) => println!("window  {w} (stated by the server)"),
        None => println!("window  not stated by the server"),
    }
    if failed > 0 {
        bail!("{name}: {failed} of 3 checks failed");
    }
    Ok(())
}
