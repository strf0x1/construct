//! The TUI's top-level coordinator: wires [`Composer`], [`Transcript`], modal
//! overlays (`PromptQueue`, `Picker`, `Browser`, `Peers`), [`Turn`] and
//! [`Motion`] together, keeping all state behind narrow methods for `lib.rs`.

mod activity;
mod clip;
mod commands;
mod events;
mod input;
mod listing;
mod restore;
mod splash;
mod status;
mod view;

use crate::composer::Composer;
use crate::motion::Motion;
use crate::overlays::ask::{Prompt, PromptQueue};
use crate::overlays::peers::{Msg as PeerMsg, PeerHub, Peers};
use crate::overlays::picker::Picker;
use crate::overlays::sessions::Browser;
use crate::theme;
use crate::transcript::{Item, Transcript};
use crate::turn::Turn;
use construct_core::agent::Event;
use construct_core::config::{Config, Paths};
use construct_core::skills::{Library, Skill};
use construct_core::tools::Catalog;
use ratatui::style::Color;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

pub(crate) struct App {
    cfg: Config,
    transcript: Transcript,
    composer: Composer,
    motion: Motion,
    turn: Turn,
    last_activity: Instant,
    started: Instant,
    /// The session's accounting, for the status line: summaries included,
    /// the same sums `/status` reads from the journal.
    totals: construct_core::journal::Totals,
    context_pct: u64,
    groups: Vec<String>,
    learning: Option<(String, Instant)>,
    /// Background jobs running, for the status line's count.
    jobs_running: usize,
    /// The session's jobs, so a session switch can see one still running.
    jobs: Option<construct_core::jobs::Jobs>,
    toast: Option<(String, Color, Instant)>,
    ready: bool,
    session: String,
    generation: String,
    canceller: Option<watch::Sender<bool>>,
    quit_armed: Option<Instant>,
    /// A ctrl-c sent a cancel during this turn. The agent may have finished
    /// before it looked (the reply is drawn before `TurnEnd` arrives), so a
    /// `TurnEnd` that says not interrupted takes the key as the quit arm.
    quit_on_end: bool,
    /// The session's tools, once the agent is built; display hints come from here.
    catalog: Option<Arc<Catalog>>,
    prompts: PromptQueue,
    picker: Option<Picker>,
    browser: Option<Browser>,
    /// Project kit, then global library. Found on the first `/`, not at
    /// startup, so the first frame never waits on it.
    skill_libs: Option<Vec<Library>>,
    peers: Peers,
}

impl App {
    pub(crate) fn new(cfg: Config) -> Self {
        Self {
            cfg,
            transcript: Transcript::default(),
            composer: Composer::default(),
            motion: Motion::from_env(),
            turn: Turn::Idle,
            last_activity: Instant::now(),
            started: Instant::now(),
            totals: Default::default(),
            context_pct: 0,
            groups: vec![],
            learning: None,
            jobs_running: 0,
            jobs: None,
            toast: None,
            ready: false,
            session: String::new(),
            generation: String::new(),
            canceller: None,
            quit_armed: None,
            quit_on_end: false,
            catalog: None,
            prompts: PromptQueue::default(),
            picker: None,
            browser: None,
            skill_libs: None,
            peers: Peers::default(),
        }
    }

    pub(crate) fn open_startup_browser(&mut self, paths: Paths, cwd: PathBuf) {
        self.browser = Some(Browser::open(paths, cwd, String::new(), true));
    }

    pub(crate) fn start_peers(&mut self, hub: Arc<dyn PeerHub>, tx: mpsc::UnboundedSender<PeerMsg>) {
        self.peers.start(hub, tx);
    }

    /// A provider's model list landed after startup: lay it under the catalog
    /// and refresh an open model picker.
    pub(crate) fn on_models_landed(&mut self, paths: &Paths) {
        construct_core::llm::overlay::lay_lists(&mut self.cfg, paths);
        if let Some(p) = self.picker.as_mut() {
            p.refresh(&self.cfg);
        }
    }

    pub(crate) fn on_turn_refused(&mut self) {
        self.turn.refused();
    }

    pub(crate) fn on_peer_msg(&mut self, msg: PeerMsg) {
        if let Some((color, text)) = self.peers.on(msg) {
            self.transcript.push(Item::Notice(color, text));
        }
    }

    pub(crate) fn push_prompt(&mut self, prompt: Prompt) {
        self.prompts.push(prompt);
    }

    pub(crate) fn on_session_event(&mut self, session: &str, ev: Event) {
        if session == self.generation {
            self.on_event(ev);
        }
    }

    pub(crate) fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(p) = self.browser.as_mut() {
            p.paste(&s);
        } else {
            self.composer.insert(&s);
        }
    }

    pub(crate) fn on_resize(&mut self) {
        self.transcript.clear_cache();
    }

    /// Open, narrow or close the slash palette to match the input.
    pub(crate) fn refresh_palette(&mut self) {
        let libs = &mut self.skill_libs;
        self.composer.refresh_palette(&self.cfg, || load_skills(libs));
    }

    pub(crate) fn animating(&self) -> bool {
        self.turn.busy()
            || self.motion.running()
            || self.browser.as_ref().is_some_and(Browser::is_loading)
            || !self.prompts.is_empty()
            || self.peers.asking()
            || self.learning.is_some()
            || self.started.elapsed() < Duration::from_millis(1600)
            || self.toast.as_ref().is_some_and(|t| t.2.elapsed() < Duration::from_secs(4))
    }

    pub(crate) fn motion_running(&self) -> bool {
        self.motion.running()
    }

    pub(crate) fn cancel(&self) {
        if let Some(c) = &self.canceller {
            c.send_replace(true);
        }
    }

    /// Every skill the user can run, read from disk now.
    fn skills(&mut self) -> Vec<Skill> {
        load_skills(&mut self.skill_libs)
    }

    /// How full the window is, as the status line says it.
    fn context_pct_now(&self) -> u64 {
        self.totals.last_prompt * 100 / (self.cfg.context_window.max(1) as u64)
    }

    /// The first ctrl-c at an idle prompt: a second within two seconds quits.
    fn arm_quit(&mut self) {
        self.quit_armed = Some(Instant::now());
        self.toast = Some(("press ctrl+c again to quit".into(), theme::YELLOW, Instant::now()));
    }
}

fn load_skills(skill_libs: &mut Option<Vec<Library>>) -> Vec<Skill> {
    let libs = skill_libs.get_or_insert_with(|| {
        let Ok(paths) = Paths::resolve() else { return vec![] };
        let cwd = std::env::current_dir().unwrap_or_default();
        let kit = construct_core::project::detect(&paths, &cwd).map(|p| Library::at(p.skills_dir()));
        kit.into_iter().chain([Library::new(&paths)]).collect()
    });
    construct_core::skills::merged(&libs.iter().collect::<Vec<_>>())
}
