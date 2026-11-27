//! What a tool call runs with: the working directory, home paths, permissions,
//! memory and skill stores, and the cancellation flag. One per session,
//! shared by every tool through an `Arc`, so per-turn counters and the output
//! cap live here.
//!
//! The design choice: a tool asks `Ctx` for narrow operations rather than
//! reaching into its fields or process globals. Capabilities owned by optional
//! tool crates or the session (`http`, `jobs`, Deck's `boundary` and `github`
//! gateway) live on their own tools and on `Agent` instead of bloating `Ctx`.

mod access;
mod knowledge;

pub(in crate::tools) use access::SHELL_STOP_AFTER;

use super::SHELL_NAME;
use crate::config::Paths;
use crate::permission::{Perms, PolicyTools};
use crate::skills::Library;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The most a tool result may hold, at any window (chars).
pub const OUTPUT_MAX: usize = 30_000;
/// One result's share of the budget. A quarter makes the cap `OUTPUT_MAX`
/// from about a 32k window up at four bytes a token (#95).
const RESULT_SHARE: f64 = 0.25;
/// A whole round's share: parallel calls split it, since the newest round is
/// never cut and ten reads at a single result's cap can fill a small window.
const ROUND_SHARE: f64 = 0.5;

pub struct Ctx {
    cwd: PathBuf,
    paths: Paths,
    cancel: watch::Receiver<bool>,
    memory: crate::memory::Store,
    skills: Library,
    /// The current repo's kit skills, when construct runs inside a repo.
    project_skills: Option<Library>,
    /// Who may be asked, and what has been approved (`permission`).
    perms: Arc<Perms>,
    read_files: Mutex<HashSet<PathBuf>>,
    /// Shell calls the sandbox blocked this turn (`shell::STOP_AFTER`).
    shell_blocked: AtomicUsize,
    /// What one result may hold now: `OUTPUT_MAX`, or less on a small
    /// window (`size_results`).
    output_max: AtomicUsize,
}

impl Ctx {
    pub fn new(
        cwd: PathBuf,
        paths: Paths,
        memory: crate::memory::Store,
        cancel: watch::Receiver<bool>,
        sandbox: crate::sandbox::Policy,
    ) -> Self {
        let tools = PolicyTools { shell: SHELL_NAME, ..Default::default() };
        let perms = Arc::new(Perms::new(&paths, sandbox).with_tools(tools));
        Self::with_perms(cwd, paths, memory, cancel, perms)
    }

    /// Construct a `Ctx` sharing an existing session `Perms` handle.
    pub(crate) fn with_perms(
        cwd: PathBuf,
        paths: Paths,
        memory: crate::memory::Store,
        cancel: watch::Receiver<bool>,
        perms: Arc<Perms>,
    ) -> Self {
        let skills = Library::new(&paths);
        let project_skills = memory.project.as_ref().map(|p| Library::at(p.skills_dir()));
        Self {
            cwd,
            paths,
            cancel,
            memory,
            skills,
            project_skills,
            perms,
            read_files: Mutex::new(HashSet::new()),
            shell_blocked: Default::default(),
            output_max: OUTPUT_MAX.into(),
        }
    }

    /// The session's working directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The construct home directory (`$CONSTRUCT_HOME`).
    pub fn home(&self) -> &Path {
        &self.paths.home
    }

    /// Resolve a tool path argument against `cwd`, expanding a leading `~/`.
    pub fn resolve_path(&self, raw: &str) -> PathBuf {
        let p = raw.trim();
        let expanded = match p.strip_prefix("~/") {
            Some(rest) => {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| PathBuf::from(p))
            }
            None => PathBuf::from(p),
        };
        if expanded.is_absolute() { expanded } else { self.cwd.join(expanded) }
    }

    /// Whether the current turn has been interrupted.
    pub fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    /// A watch receiver for `tokio::select!` or blocking progress callbacks.
    pub fn cancel_signal(&self) -> watch::Receiver<bool> {
        self.cancel.clone()
    }

    /// Size the results of a round of `calls` calls to a prompt budget of
    /// `budget_tokens`, at the bytes per token the provider's counts showed
    /// (dense data can be under two), never under a page a model can use.
    pub(crate) fn size_results(&self, budget_tokens: u64, calls: usize, bytes_per_token: f64) {
        let bytes = budget_tokens as f64 * bytes_per_token;
        let one = (bytes * RESULT_SHARE).min(bytes * ROUND_SHARE / calls.max(1) as f64) as usize;
        self.output_max.store(one.clamp(1024, OUTPUT_MAX), Ordering::Relaxed);
    }

    /// The cap a result is held to (`cap_output`, `page_output`).
    fn output_max(&self) -> usize {
        self.output_max.load(Ordering::Relaxed)
    }

    /// Take rows from 1-based line `offset` up to this round's result cap,
    /// leaving `footer_reserve` bytes for a closing summary. The first row
    /// always goes in. Returns the page and its last line number.
    pub(in crate::tools) fn page_output(
        &self,
        offset: usize,
        footer_reserve: usize,
        rows: impl Iterator<Item = String>,
    ) -> (String, usize) {
        let max = self.output_max();
        let (mut out, mut end) = (String::new(), offset - 1);
        for row in rows {
            if !out.is_empty() && out.len() + row.len() + footer_reserve > max {
                break;
            }
            out.push_str(&row);
            end += 1;
        }
        (out, end)
    }

    /// Keep tool output inside the context budget. Long output keeps its head
    /// and tail; the whole thing is spilled to a file the model can read or grep.
    pub fn cap_output(&self, kind: &str, text: &str) -> String {
        let max = self.output_max();
        if text.len() <= max {
            return text.to_string();
        }
        let dir = self.paths.spill_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{kind}-{}-{}.txt", std::process::id(), crate::clock::now_nanos()));
        let saved = std::fs::write(&path, text).is_ok();
        let head = crate::text::truncate(text, max / 2);
        let lines = text.lines().count();
        format!(
            "{head}\n\n[… {} bytes, {lines} lines total{} …]\n\n{}",
            text.len(),
            if saved { format!("; full output in {}", path.display()) } else { String::new() },
            crate::text::tail(text, max / 3)
        )
    }

    /// A new user turn: per-turn counts start over.
    pub(crate) fn turn_began(&self) {
        self.shell_blocked.store(0, Ordering::Relaxed);
    }

    /// Shell calls blocked so far this turn.
    fn shell_blocks(&self) -> usize {
        self.shell_blocked.load(Ordering::Relaxed)
    }

    /// Count one more blocked shell call; the new total.
    fn shell_blocked(&self) -> usize {
        self.shell_blocked.fetch_add(1, Ordering::Relaxed) + 1
    }

    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    pub(in crate::tools) fn mark_read(&self, p: &Path) {
        self.read_files.lock().unwrap().insert(p.to_path_buf());
    }

    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    pub(in crate::tools) fn was_read(&self, p: &Path) -> bool {
        self.read_files.lock().unwrap().contains(p)
    }
}
