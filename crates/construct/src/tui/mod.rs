//! Full-screen TUI. The first frame is drawn before the agent is built, every
//! frame is a synchronized update (no tearing), and frames are drawn only
//! when something changed: the animation tick runs only while work is
//! visibly in progress.

mod app;
mod args;
mod ask;
mod boxed;
mod clip;
mod help;
mod input;
mod jobs;
mod listing;
mod markdown;
mod palette;
mod picker;
mod prompt;
mod status;
mod theme;
mod view;
mod worker;

use crate::agent::Event;
use crate::config::{Config, Paths};
use anyhow::Result;
use app::{App, Block, Cmd, Kind};
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

pub async fn run(cfg: Config, paths: Paths, resume: Option<String>) -> Result<()> {
    let saved_stderr = quiet_stderr(&paths);
    let r = run_inner(cfg, paths, resume).await;
    restore_stderr(saved_stderr);
    r
}

async fn run_inner(cfg: Config, paths: Paths, resume: Option<String>) -> Result<()> {
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
    app.draw(&mut term)?;
    crate::trace::mark("first frame");

    // Build the agent off the UI path; the first frame is already up.
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<Ready>();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<ask::Prompt>();
    let worker = tokio::spawn(agent_task(cfg.clone(), paths.clone(), resume, cmd_rx, ev_tx, ready_tx, prompt_tx));

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = false;
    let mut last_draw = Instant::now();
    loop {
        let animating = app.animating();
        tokio::select! {
            Some(ready) = ready_rx.recv() => {
                let (session, canceller, groups, history, catalog, spent) = ready;
                app.ready = true;
                app.totals = spent;
                app.session = session;
                app.canceller = Some(canceller);
                app.groups = groups;
                app.catalog = Some(catalog);
                // The worker warmed the list before building the agent; this is a file read.
                crate::llm::catalog::overlay(&mut app.cfg, &paths);
                // A `/model …` typed before now was matched without the catalog.
                app.palette = None;
                app.refresh_palette();
                for m in history {
                    match m.role.as_str() {
                        "user" => app.push(Block::new(Kind::User, m.content.unwrap_or_default().split("\n\n<system-reminder>").next().unwrap_or("").to_string())),
                        "assistant" if m.content.is_some() => app.push(Block::new(Kind::Assistant, m.content.unwrap_or_default())),
                        _ => {}
                    }
                }
                dirty = true;
            }
            Some(prompt) = prompt_rx.recv() => {
                app.prompts.push_back(prompt);
                dirty = true;
            }
            Some(ev) = ev_rx.recv() => {
                if let Event::Error { message } = &ev
                    && message.starts_with("fatal:") {
                        app.notice(theme::RED, message.clone());
                    }
                if let Event::Route { groups, .. } = &ev {
                    for g in groups { if !app.groups.contains(g) { app.groups.push(g.clone()); } }
                }
                app.on_event(ev);
                // batch whatever else is already queued before drawing
                while let Ok(ev) = ev_rx.try_recv() { app.on_event(ev); }
                dirty = true;
            }
            Some(Ok(ev)) = events.next() => {
                match ev {
                    CEvent::Key(k) if k.kind != KeyEventKind::Release => {
                        if handle_key(&mut app, k, &cmd_tx) { break; }
                    }
                    CEvent::Paste(s) => { let s = s.replace("\r\n", "\n").replace('\r', "\n"); app.insert(&s); }
                    CEvent::Resize(..) => { app.cache.clear(); }
                    _ => continue,
                }
                app.refresh_palette();
                dirty = true;
            }
            _ = tick.tick(), if animating => { dirty = true; }
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
