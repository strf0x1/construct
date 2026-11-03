//! What a tool call runs with: the working directory, the home, the endpoint
//! client, permissions, jobs and the cancellation flag. One per session,
//! shared by every tool through an `Arc`, so per-turn counters live here.
//!
//! The design choice: a tool asks this struct for the world; it never reaches
//! for process globals. That is what makes the catalog and the tools testable
//! without a terminal or a model.

use crate::config::Paths;
use crate::skills::Library;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub struct Ctx {
    pub cwd: PathBuf,
    pub paths: Paths,
    pub http: reqwest::Client,
    pub cancel: watch::Receiver<bool>,
    pub memory: crate::memory::Store,
    pub skills: Library,
    /// The current repo's kit skills, when construct runs inside a repo.
    pub project_skills: Option<Library>,
    pub mcp: Option<Arc<crate::mcp::Manager>>,
    pub sandbox: crate::sandbox::Policy,
    /// Who may be asked, and what has been approved (`permission`).
    pub perms: Arc<crate::permission::Perms>,
    /// Work that outlives a call and reports when it ends.
    pub jobs: crate::jobs::Jobs,
    /// Deck's boundary status endpoint, when this process runs in a Deck
    /// task. Absent everywhere else, which is why the boundary tools are
    /// absent from the catalog too.
    pub boundary: Option<Arc<super::boundary::Boundary>>,
    read_files: Mutex<HashSet<PathBuf>>,
    /// Shell calls the sandbox blocked this turn (`shell::STOP_AFTER`).
    shell_blocked: std::sync::atomic::AtomicUsize,
    /// What one result may hold now: `OUTPUT_MAX`, or less on a small
    /// window (`size_results`).
    pub(super) output_max: std::sync::atomic::AtomicUsize,
}

impl Ctx {
    pub fn new(
        cwd: PathBuf,
        paths: Paths,
        memory: crate::memory::Store,
        cancel: watch::Receiver<bool>,
        mcp: Option<Arc<crate::mcp::Manager>>,
        sandbox: crate::sandbox::Policy,
    ) -> Self {
        #[expect(
            clippy::expect_used,
            reason = "fails only when the TLS backend can't start, and no tool works without it"
        )]
        let http = reqwest::Client::builder()
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("http client");
        let skills = Library::new(&paths);
        let project_skills = memory.project.as_ref().map(|p| Library::at(p.skills_dir()));
        let perms = Arc::new(crate::permission::Perms::new(&paths, sandbox.clone()));
        Self {
            cwd,
            paths,
            http,
            cancel,
            memory,
            skills,
            project_skills,
            mcp,
            sandbox,
            perms,
            jobs: crate::jobs::Jobs::new(&Default::default()),
            boundary: None,
            read_files: Mutex::new(HashSet::new()),
            shell_blocked: Default::default(),
            output_max: super::OUTPUT_MAX.into(),
        }
    }
    /// This session's jobs, under the limit its config sets.
    pub fn with_jobs(mut self, jobs: crate::jobs::Jobs) -> Self {
        self.jobs = jobs;
        self
    }
    /// The Deck boundary endpoint, when the VM runtime set one.
    pub fn with_boundary(mut self, boundary: Option<Arc<super::boundary::Boundary>>) -> Self {
        self.boundary = boundary;
        self
    }
    /// The project kit (when there is one) and then the global library: the
    /// order a name is looked up in, so a kit skill hides a global one.
    pub fn skill_libs(&self) -> Vec<&Library> {
        self.project_skills.iter().chain(std::iter::once(&self.skills)).collect()
    }
    /// The library that holds `name`, kit first.
    pub fn skill_lib(&self, name: &str) -> Option<&Library> {
        self.skill_libs().into_iter().find(|l| l.get(name).is_some())
    }
    /// Every skill the model or the user can reach, read from disk now.
    pub fn all_skills(&self) -> Vec<crate::skills::Skill> {
        crate::skills::merged(&self.skill_libs())
    }
    /// A new user turn: per-turn counts start over.
    pub fn turn_began(&self) {
        self.shell_blocked.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    /// Shell calls blocked so far this turn.
    pub fn shell_blocks(&self) -> usize {
        self.shell_blocked.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Count one more blocked shell call; the new total.
    pub fn shell_blocked(&self) -> usize {
        self.shell_blocked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }
    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    pub fn mark_read(&self, p: &Path) {
        self.read_files.lock().unwrap().insert(p.to_path_buf());
    }
    #[expect(clippy::unwrap_used, reason = "poisoned lock: a panic elsewhere")]
    pub fn was_read(&self, p: &Path) -> bool {
        self.read_files.lock().unwrap().contains(p)
    }
}
