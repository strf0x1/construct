//! The parent's side of a delegation: start the child under its sandbox, hand
//! it the task, pass its events up, and stop it on timeout or cancel. A child
//! always ends: it finishes its turn, or is cancelled and given a moment to
//! wrap up, or is stopped the way the sandbox helper expects (SIGTERM, then
//! the group). Its scratch directory is removed when it is gone.

use super::wire::{Jsonl, Origin, Pending, Replier, ToChild, Wire};
use crate::agents::{AgentDef, Protocol};
use crate::config::Config;
use crate::event::Event;
use crate::permission::{Decision, Perms};
use crate::sandbox::{GRACE, Policy, StopOnDrop};
use crate::tools::Call;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

/// How much of the child's stderr to keep for an explanation.
const STDERR_TAIL: usize = 2048;

pub struct Spec<'a> {
    pub name: &'a str,
    pub def: &'a AgentDef,
    pub config: Config,
    pub system: String,
    /// What the child's process is allowed, from `Policy::for_agent`.
    pub policy: Policy,
    pub cwd: &'a Path,
    /// The parent's own endpoint config, to resolve the secrets it names.
    pub parent: &'a Config,
    /// Where the child's questions for the user are decided and remembered.
    pub perms: Arc<Perms>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Timeout,
    Interrupted,
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// The child's last answer.
    pub summary: String,
    /// The child's session id, once its head arrived.
    pub session: Option<String>,
    pub steps: u32,
    pub prompt: u64,
    pub completion: u64,
    pub stopped: Option<Stop>,
    /// Why it failed, when it did without being stopped.
    pub error: Option<String>,
    /// The child's turn is over; the parent closes its input.
    pub finished: bool,
    /// The tail of the child's stderr, for explaining a stop or a failure.
    pub diagnostics: String,
    /// Questions the wire just read that wait for the user; the loop takes them.
    pub asks: Vec<Pending>,
}

impl Outcome {
    /// What the parent keeps of the child's stream, apart from passing it up.
    pub fn note(&mut self, ev: &Event) {
        match ev {
            Event::Head { id, .. } => self.session = Some(id.clone()),
            Event::Assistant { message } if message.tool_calls.is_empty() => {
                if let Some(text) = &message.content {
                    self.summary = text.clone();
                }
            }
            Event::Error { message } => self.error = Some(message.clone()),
            Event::TurnEnd { steps, prompt, completion, .. } => {
                self.steps = *steps;
                self.prompt = *prompt;
                self.completion = *completion;
                self.finished = true;
            }
            _ => {}
        }
    }
}

/// The program to run, its arguments, and the conversation to have with it.
type Plan = (String, Vec<String>, Box<dyn Wire>);

fn plan(spec: &Spec<'_>, task: &str, call: &Call) -> Result<Plan, String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find construct's own binary: {e}"))?;
    let exe = exe.display().to_string();
    let protocol = spec.def.protocol(spec.name).map_err(|e| format!("{e:#}"))?;
    let origin = Origin {
        policy: spec.policy.clone(),
        system: spec.system.clone(),
        parent_session: call.session().unwrap_or_default().to_string(),
        parent_call_id: call.id.clone(),
    };
    Ok(match (protocol, spec.def.command.split_first()) {
        (Protocol::Jsonl, _) => {
            let start = ToChild::Start {
                config: Box::new(spec.config.clone()),
                cwd: spec.cwd.to_path_buf(),
                origin: Box::new(origin),
            };
            (exe, vec!["__child".into()], Box::new(Jsonl::new(start, task)))
        }
        (Protocol::Acp, None) => {
            let start = crate::acp::Start { config: Box::new(spec.config.clone()), origin: Box::new(origin) };
            let wire = crate::acp::client::Client::construct(spec.cwd, task, start, spec.def.gate());
            (exe, vec!["__child".into(), "--acp".into()], Box::new(wire))
        }
        (Protocol::Acp, Some((prog, args))) => {
            let wire = crate::acp::client::Client::foreign(spec.name, spec.parent, spec.cwd, task, spec.def.gate());
            (prog.clone(), args.to_vec(), Box::new(wire))
        }
    })
}

pub async fn run(
    spec: Spec<'_>,
    task: &str,
    call: &Call,
    mut cancel: watch::Receiver<bool>,
) -> Result<Outcome, String> {
    let tmp = spec.policy.tmp.clone();
    let out = drive(spec, task, call, &mut cancel).await;
    let _ = std::fs::remove_dir_all(&tmp);
    out
}

async fn drive(spec: Spec<'_>, task: &str, call: &Call, cancel: &mut watch::Receiver<bool>) -> Result<Outcome, String> {
    let (prog, args, mut wire) = plan(&spec, task, call)?;
    // Network and signals only: the child's tools are confined per call.
    let mut cmd = spec.policy.net_only().spawn(&prog, &args);
    cmd.current_dir(spec.cwd)
        .env("CONSTRUCT_HOME", spec.policy.tmp.join("home"))
        .env(super::DEPTH_ENV, (super::depth() + 1).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    cmd.envs(&spec.def.env);
    // Only the secrets the definition names. A name that isn't set is left out.
    for name in spec.def.secret_names(spec.parent) {
        if let Some(v) = crate::secrets::get(&name) {
            cmd.env(name, v);
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("starting the child failed: {e}"))?;
    let mut guard = StopOnDrop(child.id());
    let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("starting the child failed: no pipes".into());
    };
    write_all(&mut stdin, wire.open()).await.map_err(|e| format!("the child closed its input: {e}"))?;
    let mut stdin = Some(stdin);

    // Kept while the child runs, or a chatty child would block on a full pipe.
    let tail = tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut chunk).await
            && n > 0
        {
            buf.extend_from_slice(&chunk[..n]);
            let over = buf.len().saturating_sub(STDERR_TAIL);
            buf.drain(..over);
        }
        String::from_utf8_lossy(&buf).trim().to_string()
    });

    let mut out = Outcome::default();
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::sleep(Duration::from_secs(spec.def.timeout));
    tokio::pin!(deadline);
    let never = Duration::from_secs(60 * 60 * 24 * 365);
    let hard = tokio::time::sleep(never);
    tokio::pin!(hard);
    // Questions for the user. Each is bounded by `over`, which is set when the
    // delegation is stopped or its turn ends, so a prompt never outlives the
    // call that raised it and the child hears `cancelled`.
    let (over_tx, over) = watch::channel(false);
    let mut waiting: FuturesUnordered<BoxFuture<'static, (Replier, Decision)>> = FuturesUnordered::new();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    let reply = wire.line(&l, call, &mut out);
                    if let Some(w) = stdin.as_mut() {
                        let _ = write_all(w, reply).await;
                    }
                    for p in out.asks.drain(..) {
                        waiting.push(ask(spec.perms.clone(), p, spec.name, over.clone()));
                    }
                    if out.finished {
                        over_tx.send_replace(true);
                    }
                    if out.finished && stdin.take().is_some() {
                        // Done with the child: closing its input ends it. One
                        // that doesn't take the hint (some ACP adapters keep
                        // running) is stopped after a moment: its work is
                        // done, so this is not a timeout.
                        hard.as_mut().reset(tokio::time::Instant::now() + GRACE);
                    }
                }
                Ok(None) | Err(_) => break,
            },
            Some((replier, decision)) = waiting.next(), if !waiting.is_empty() => {
                let reply = replier(decision);
                for ev in reply.events {
                    out.note(&ev);
                    call.forward(ev);
                }
                if let Some(w) = stdin.as_mut() {
                    let _ = write_all(w, reply.lines).await;
                }
            }
            _ = &mut deadline, if out.stopped.is_none() && !out.finished => {
                over_tx.send_replace(true);
                out.stopped = Some(Stop::Timeout);
                cancel_child(&mut stdin, wire.cancel()).await;
                hard.as_mut().reset(tokio::time::Instant::now() + GRACE);
            }
            r = cancel.changed(), if out.stopped.is_none() && !out.finished => {
                if r.is_ok() && *cancel.borrow() {
                    over_tx.send_replace(true);
                    out.stopped = Some(Stop::Interrupted);
                    cancel_child(&mut stdin, wire.cancel()).await;
                    hard.as_mut().reset(tokio::time::Instant::now() + GRACE);
                }
            }
            _ = &mut hard, if out.stopped.is_some() || out.finished => {
                // It didn't wrap up: stop it the way the helper expects.
                guard.stop();
                break;
            }
        }
    }
    drop(stdin);
    let status = match tokio::time::timeout(GRACE, child.wait()).await {
        Ok(s) => s.ok(),
        Err(_) => {
            guard.stop();
            match tokio::time::timeout(GRACE, child.wait()).await {
                Ok(s) => s.ok(),
                Err(_) => {
                    guard.kill_group();
                    child.wait().await.ok()
                }
            }
        }
    };
    guard.0 = None;
    let diagnostics = tokio::time::timeout(GRACE, tail).await.ok().and_then(Result::ok).unwrap_or_default();
    out.diagnostics = diagnostics.clone();
    if !out.finished && out.stopped.is_none() && out.error.is_none() {
        let how = match status.and_then(|s| s.code()) {
            Some(c) => format!("exit {c}"),
            None => "killed by a signal".into(),
        };
        out.error = Some(if diagnostics.is_empty() {
            format!("the child ended without finishing ({how})")
        } else {
            format!("the child ended without finishing ({how}): {diagnostics}")
        });
    }
    Ok(out)
}

/// One of the child's questions, as a future of its answer: decided by
/// `perms` (which asks the user), or cancelled when the delegation is over.
fn ask(
    perms: Arc<Perms>,
    p: Pending,
    agent: &str,
    mut over: watch::Receiver<bool>,
) -> BoxFuture<'static, (Replier, Decision)> {
    let request = p.request.from_agent(agent);
    Box::pin(async move {
        let d = tokio::select! {
            biased;
            _ = stopped(&mut over) => Decision::CANCEL,
            d = perms.decide(request) => d,
        };
        (p.reply, d)
    })
}

async fn stopped(over: &mut watch::Receiver<bool>) {
    loop {
        if *over.borrow() {
            return;
        }
        if over.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn cancel_child(stdin: &mut Option<tokio::process::ChildStdin>, lines: Vec<String>) {
    if let Some(w) = stdin {
        let _ = write_all(w, lines).await;
    }
}

async fn write_all(w: &mut tokio::process::ChildStdin, lines: Vec<String>) -> std::io::Result<()> {
    for l in lines {
        w.write_all(l.as_bytes()).await?;
    }
    w.flush().await
}
