//! Full-screen TUI. The first frame is drawn before the agent is built, every
//! frame is a synchronized update (no tearing), and frames are drawn only
//! when something changed: the animation tick runs only while work is
//! visibly in progress.

mod app;
mod boxed;
mod composer;
mod help;
mod markdown;
mod motion;
mod overlays;
mod table;
mod text;
mod theme;
mod transcript;
mod turn;
mod worker;

use anyhow::Result;
use app::App;
use construct_core::agent::Event;
use construct_core::config::{Config, Paths};
use construct_core::tools::ToolSources;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as CEvent, EventStream, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use overlays::ask::Prompt;
use overlays::peers::Msg as PeerMsg;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Write;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use worker::{Cmd, Launch, Out, Ready, agent_task};

pub use overlays::peers::{Act, PeerHub, PeerRow, Question, Standing};

struct Guard {
    kbd: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore(self.kbd);
    }
}

/// Alternate-scroll mode: in the alternate screen the terminal turns the wheel
/// into up/down keys, so scrolling works without mouse capture and the
/// terminal keeps native selection. Reset on every exit path.
const ALT_SCROLL_ON: &str = "\x1b[?1007h";
const ALT_SCROLL_OFF: &str = "\x1b[?1007l";

fn restore(kbd: bool) {
    let mut out = std::io::stdout();
    if kbd {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = out.write_all(ALT_SCROLL_OFF.as_bytes());
    let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = terminal::disable_raw_mode();
}

/// Point fd 2 at a log file while the TUI owns the terminal, so a library
/// writing to stderr can't scribble over the screen. Returns the saved fd.
fn quiet_stderr(paths: &Paths) -> Option<i32> {
    use std::os::fd::AsRawFd;
    let _ = std::fs::create_dir_all(&paths.home);
    let f = std::fs::OpenOptions::new().create(true).append(true).open(paths.home.join("stderr.log")).ok()?;
    // SAFETY: dup/dup2 on valid descriptors; the file outlives the call via dup2.
    unsafe {
        let saved = libc::dup(2);
        if saved < 0 || libc::dup2(f.as_raw_fd(), 2) < 0 {
            return None;
        }
        Some(saved)
    }
}

fn restore_stderr(saved: Option<i32>) {
    if let Some(fd) = saved {
        // SAFETY: restoring the descriptor saved by quiet_stderr.
        unsafe {
            libc::dup2(fd, 2);
            libc::close(fd);
        }
    }
}

/// Fold in the agent events that have already arrived, so the next decision is
/// made on the state the agent has reported. A reply is drawn before its
/// `TurnEnd` is handled here; a ctrl-c or a session switch pressed in that gap
/// would otherwise be judged against a turn that has already finished (and for
/// ctrl-c, the interrupt it sends is a no-op, so quit never arms).
fn fold_events(app: &mut App, rx: &mut mpsc::UnboundedReceiver<(String, Event)>) {
    while let Ok((session, ev)) = rx.try_recv() {
        app.on_session_event(&session, ev);
    }
}

pub async fn run(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    startup_browser: bool,
    model_override: Option<String>,
    sources: ToolSources,
    hub: Option<std::sync::Arc<dyn PeerHub>>,
) -> Result<()> {
    let saved_stderr = quiet_stderr(&paths);
    let r = run_inner(cfg, paths, resume, startup_browser, model_override, sources, hub).await;
    restore_stderr(saved_stderr);
    r
}

async fn run_inner(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    startup_browser: bool,
    model_override: Option<String>,
    sources: ToolSources,
    hub: Option<std::sync::Arc<dyn PeerHub>>,
) -> Result<()> {
    construct_core::trace::mark("tui start");
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    stdout.write_all(ALT_SCROLL_ON.as_bytes())?;
    stdout.flush()?;
    // Push unconditionally: asking the terminal first costs up to 2s on
    // terminals that never answer, and the flags are ignored where unsupported.
    let kbd = true;
    let _ = execute!(stdout, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    let _guard = Guard { kbd };
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(kbd);
        hook(info);
    }));
    // No term.clear(): it asks the terminal for the cursor position (a round
    // trip), and a freshly entered alternate screen is already blank.
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut app = App::new(cfg.clone());
    if startup_browser {
        app.open_startup_browser(paths.clone(), std::env::current_dir()?);
    }
    app.draw(&mut term)?;
    construct_core::trace::mark("first frame");

    // Build the agent off the UI path; the first frame is already up.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<(String, Event)>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<Ready>();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<Prompt>();
    // A provider's model list landing, maybe after the session started.
    let (landed_tx, mut landed_rx) = mpsc::unbounded_channel::<()>();
    let (refused_tx, mut refused_rx) = mpsc::unbounded_channel::<()>();
    let out = Out { tx: ev_tx, ready: ready_tx, prompts: prompt_tx, landed: landed_tx, refused: refused_tx };
    let worker = tokio::spawn(agent_task(
        cfg.clone(),
        paths.clone(),
        Launch { resume, startup_browser, model_override, sources },
        cmd_rx,
        out,
    ));

    // The hub is polled only now that the first frame is up.
    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel::<PeerMsg>();
    if let Some(hub) = hub {
        app.start_peers(hub, peer_tx);
    }
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = false;
    let mut last_draw = Instant::now();
    loop {
        let animating = app.animating();
        tokio::select! {
            Some(()) = landed_rx.recv() => {
                // A file read: the fetch is done. The running model's window stays the agent's.
                app.on_models_landed(&paths);
                dirty = true;
            }
            Some(ready) = ready_rx.recv() => {
                app.restore(ready);
                dirty = true;
            }
            Some(()) = refused_rx.recv() => {
                app.on_turn_refused();
                dirty = true;
            }
            Some(msg) = peer_rx.recv() => {
                app.on_peer_msg(msg);
                dirty = true;
            }
            Some(prompt) = prompt_rx.recv() => {
                app.push_prompt(prompt);
                dirty = true;
            }
            Some((session, ev)) = ev_rx.recv() => {
                app.on_session_event(&session, ev);
                fold_events(&mut app, &mut ev_rx);
                dirty = true;
            }
            Some(Ok(ev)) = events.next() => {
                match ev {
                    CEvent::Key(k) if k.kind != KeyEventKind::Release => {
                        fold_events(&mut app, &mut ev_rx);
                        if app.handle_key(k, &cmd_tx) { break; }
                    }
                    CEvent::Paste(s) => app.paste(&s),
                    CEvent::Resize(..) => app.on_resize(),
                    _ => continue,
                }
                app.refresh_palette();
                dirty = true;
            }
            _ = tick.tick(), if animating => { dirty = true; }
            // motion (a comet, an unfurl) wants ~30fps; it runs only while something moves
            _ = tokio::time::sleep(Duration::from_millis(33)), if app.motion_running() => { dirty = true; }
            // a frame held back by the redraw cap still gets drawn
            _ = tokio::time::sleep_until((last_draw + Duration::from_millis(8)).into()), if dirty => {}
        }
        // Cap redraws at ~120fps; a burst of deltas becomes one frame.
        if dirty && last_draw.elapsed() >= Duration::from_millis(8) {
            app.draw(&mut term)?;
            last_draw = Instant::now();
            dirty = false;
        }
    }
    app.cancel();
    drop(cmd_tx);
    drop(_guard);
    let _ = tokio::time::timeout(Duration::from_millis(1500), worker).await;
    Ok(())
}
