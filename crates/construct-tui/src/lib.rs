//! Full-screen TUI. The first frame is drawn before the agent is built, every
//! frame is a synchronized update (no tearing), and frames are drawn only
//! when something changed: the animation tick runs only while work is
//! visibly in progress.

mod activity;
mod app;
mod app_events;
mod args;
mod ask;
mod boxed;
mod children;
mod clip;
mod commands;
mod dock;
mod help;
mod input;
mod jobs;
mod learning;
mod listing;
mod markdown;
mod palette;
mod peek;
mod picker;
mod prompt;
mod restore;
mod sessions;
mod status;
mod table;
mod theme;
mod turn;
mod view;
mod worker;

use crate::agent::Event;
use crate::config::{Config, Paths};
use anyhow::Result;
use app::{App, Cmd};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as CEvent, EventStream, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use input::handle_key;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Write;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use worker::{Ready, agent_task};

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
        if session == app.generation {
            app.on_event(ev);
        }
    }
}

pub async fn run(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    startup_browser: bool,
    model_override: Option<String>,
) -> Result<()> {
    let saved_stderr = quiet_stderr(&paths);
    let r = run_inner(cfg, paths, resume, startup_browser, model_override).await;
    restore_stderr(saved_stderr);
    r
}

async fn run_inner(
    cfg: Config,
    paths: Paths,
    resume: Option<String>,
    startup_browser: bool,
    model_override: Option<String>,
) -> Result<()> {
    crate::trace::mark("tui start");
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
        app.browser = Some(sessions::Browser::open(paths.clone(), std::env::current_dir()?, String::new(), true));
    }
    app.draw(&mut term)?;
    crate::trace::mark("first frame");

    // Build the agent off the UI path; the first frame is already up.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<(String, Event)>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<Ready>();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<ask::Prompt>();
    // A provider's model list landing, maybe after the session started.
    let (landed_tx, mut landed_rx) = mpsc::unbounded_channel::<()>();
    let (refused_tx, mut refused_rx) = mpsc::unbounded_channel::<()>();
    let out = worker::Out { tx: ev_tx, ready: ready_tx, prompts: prompt_tx, landed: landed_tx, refused: refused_tx };
    let worker =
        tokio::spawn(agent_task(cfg.clone(), paths.clone(), resume, startup_browser, model_override, cmd_rx, out));

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // A child on the dock only needs its elapsed time and spinner now and then.
    let mut slow = tokio::time::interval(Duration::from_secs(1));
    slow.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = false;
    let mut last_draw = Instant::now();
    loop {
        let animating = app.animating();
        let docked = app.dock.active();
        tokio::select! {
            Some(()) = landed_rx.recv() => {
                // A file read: the fetch is done. The running model's window stays the agent's.
                crate::llm::overlay::lay_lists(&mut app.cfg, &paths);
                if let Some(p) = app.picker.as_mut() {
                    p.refresh(&app.cfg);
                }
                dirty = true;
            }
            Some(ready) = ready_rx.recv() => {
                app.restore(ready);
                dirty = true;
            }
            Some(()) = refused_rx.recv() => {
                app.turn.refused();
                dirty = true;
            }
            Some(prompt) = prompt_rx.recv() => {
                app.prompts.push_back(prompt);
                dirty = true;
            }
            Some((session, ev)) = ev_rx.recv() => {
                if session == app.generation { app.on_event(ev); }
                fold_events(&mut app, &mut ev_rx);
                dirty = true;
            }
            Some(Ok(ev)) = events.next() => {
                match ev {
                    CEvent::Key(k) if k.kind != KeyEventKind::Release => {
                        fold_events(&mut app, &mut ev_rx);
                        if handle_key(&mut app, k, &cmd_tx) { break; }
                    }
                    CEvent::Paste(s) => { let s = s.replace("\r\n", "\n").replace('\r', "\n"); if let Some(p) = app.browser.as_mut() { p.paste(&s); } else { app.insert(&s); } }
                    CEvent::Resize(..) => { app.cache.clear(); }
                    _ => continue,
                }
                app.refresh_palette();
                dirty = true;
            }
            _ = tick.tick(), if animating => { dirty = true; }
            _ = slow.tick(), if docked => { dirty = true; }
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
    if let Some(c) = &app.canceller {
        c.send_replace(true);
    }
    drop(cmd_tx);
    drop(_guard);
    let _ = tokio::time::timeout(Duration::from_millis(1500), worker).await;
    Ok(())
}
