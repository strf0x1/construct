//! `Perms`: one per agent session, between the tools and whoever can answer.
//!
//! Order of events for a blocked path: the sandbox policy widened by what the
//! session has already approved says no; the path is not protected; the user
//! is asked (racing the turn's cancel, so Esc dismisses the question); the
//! answer is applied (once: only this check passes; session: remembered;
//! always: also written to `config.toml`) and journaled as `Event::Access`.
//! Config the user wrote by hand is already inside `Policy`, so it never
//! reaches here.

use super::grants::{Grants, Roots, folder_for, in_construct_home, protected};
use super::persist;
use super::{Access, Ask, Decision, NoAsk, PathAsk, Request, ToolAsk, Verdict};
use crate::config::Paths;
use crate::event::{Event, Events};
use crate::journal::Writer;
use crate::sandbox::Policy;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::WeakUnboundedSender;
use tokio::sync::watch;

pub struct Perms {
    paths: Paths,
    /// The session's sandbox policy before any approval.
    base: Policy,
    ask: Mutex<Arc<dyn Ask>>,
    grants: Mutex<Grants>,
    /// Weak: a sender kept here would hold the event stream open, and the
    /// printer at the other end waits for every sender to be gone.
    sink: Mutex<Option<(Arc<Writer>, WeakUnboundedSender<Event>)>>,
}

/// `fut`, unless the turn is cancelled first.
async fn raced(fut: impl Future<Output = Decision>, mut cancel: watch::Receiver<bool>) -> Decision {
    tokio::pin!(fut);
    loop {
        if *cancel.borrow() {
            return Decision::CANCEL;
        }
        tokio::select! {
            d = &mut fut => return d,
            r = cancel.changed() => if r.is_err() { return fut.await },
        }
    }
}

impl Perms {
    pub fn new(paths: &Paths, base: Policy) -> Self {
        Self {
            paths: paths.clone(),
            base,
            ask: Mutex::new(Arc::new(NoAsk)),
            grants: Mutex::new(Grants::default()),
            sink: Mutex::new(None),
        }
    }

    /// Who answers from now on (the TUI's prompt, or a parent process).
    pub fn set_ask(&self, ask: Arc<dyn Ask>) {
        *self.ask.lock().unwrap_or_else(|e| e.into_inner()) = ask;
    }

    /// Where decisions are recorded; set at the start of each turn.
    pub fn set_sink(&self, journal: Arc<Writer>, tx: Events) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some((journal, tx.downgrade()));
    }

    fn asker(&self) -> Arc<dyn Ask> {
        // A poisoned lock means a panic mid-assignment; the value is still usable.
        self.ask.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn grants(&self) -> std::sync::MutexGuard<'_, Grants> {
        self.grants.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, ev: Event) {
        if let Some((journal, tx)) = &*self.sink.lock().unwrap_or_else(|e| e.into_inner())
            && let Some(tx) = tx.upgrade()
        {
            journal.emit(ev, &tx);
        }
    }

    /// The sandbox policy plus what this session has approved for its own agent.
    pub fn policy(&self) -> Policy {
        let r = self.grants().roots("");
        self.base.widened(&r.read, &r.write)
    }

    /// What this session approved for a delegated agent, to add to the next
    /// policy it runs under.
    pub fn roots_for(&self, agent: &str) -> Roots {
        self.grants().roots(agent)
    }

    /// The roots a decision grants: the folder only if the request's own
    /// folder is one `folder_for` would offer for that path (a child's
    /// request is not taken at its word).
    fn granted(req: &PathAsk, d: Decision) -> Vec<PathBuf> {
        let paths: Vec<PathBuf> = req.paths.iter().map(PathBuf::from).collect();
        if d.folder
            && let ([only], Some(folder)) = (paths.as_slice(), &req.folder)
            && folder_for(only).as_deref() == Some(Path::new(folder))
        {
            return vec![PathBuf::from(folder)];
        }
        paths
    }

    /// Remember (and for `Always`, write) what `d` granted. A failure to
    /// write is reported, and the grant still holds for the session.
    fn apply(&self, req: &PathAsk, d: Decision, roots: &[PathBuf]) -> Option<String> {
        if !matches!(d.verdict, Verdict::Session | Verdict::Always) {
            return None;
        }
        let mut g = self.grants();
        for r in roots {
            g.add_path(&req.agent, req.access, r.clone());
        }
        drop(g);
        if d.verdict == Verdict::Always {
            for r in roots {
                if let Err(e) = persist::allow_path(&self.paths, &req.agent, req.access == Access::Write, r) {
                    return Some(format!("couldn't save {} to config.toml: {e:#}", r.display()));
                }
            }
        }
        None
    }

    async fn ask_paths(&self, req: PathAsk, cancel: watch::Receiver<bool>) -> Option<(Verdict, Vec<PathBuf>)> {
        let ask = self.asker();
        if !ask.interactive() {
            return None;
        }
        let d = raced(ask.ask(Request::Path(req.clone())), cancel).await;
        let roots = Self::granted(&req, d);
        if let Some(error) = self.apply(&req, d, &roots) {
            self.record(Event::Error { message: error });
        }
        self.record(Event::Access {
            agent: req.agent.clone(),
            tool: req.tool.clone(),
            access: req.access.word().into(),
            paths: roots.iter().map(|p| p.display().to_string()).collect(),
            decision: d.verdict.word().into(),
        });
        Some((d.verdict, roots))
    }

    /// Let a file tool reach `path`, asking if the sandbox says no. The error
    /// is the model's: why, and who declined.
    pub async fn path(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), String> {
        self.path_asked(tool, access, path, cancel).await.map(|_| ())
    }

    /// `path`, saying what the user approved: the real path when the sandbox
    /// said no and the user said yes (possibly for this call only, so it is
    /// not in `policy()`), None when the policy already allowed it. The file
    /// tools open under exactly that, never under a second look at the name.
    pub async fn path_asked(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<PathBuf>, String> {
        let policy = self.policy();
        let denied = match access {
            Access::Read => policy.check_read(path),
            Access::Write => policy.check_write(path),
        };
        let Err(msg) = denied else { return Ok(None) };
        let real = Policy::real(path);
        if in_construct_home(&real) {
            use crate::tools::{history::JournalQuery, memory::Memory, skill::SkillManage, skill::SkillView};
            return Err(format!(
                "{msg} That is construct's own home: use the {}, {}, {} and {} tools for what lives there.",
                SkillView::NAME,
                SkillManage::NAME,
                Memory::NAME,
                JournalQuery::NAME
            ));
        }
        if protected(&real) {
            return Err(format!("{msg} That place is protected; only a path typed into config.toml opens it."));
        }
        let req = PathAsk {
            agent: String::new(),
            tool: tool.into(),
            access,
            folder: folder_for(&real).map(|f| f.display().to_string()),
            paths: vec![real.display().to_string()],
            detail: String::new(),
            rerun: false,
            ahead: false,
            no_always: false,
        };
        match self.ask_paths(req, cancel).await {
            Some((v, _)) if v.allows() => Ok(Some(real)),
            Some((Verdict::Deny, _)) => Err(format!("{msg} The user was asked and declined.")),
            _ => Err(msg),
        }
    }

    /// A shell command names paths outside the policy (`ahead`: before it
    /// runs), or failed on them: ask whether to run it with them. The paths to
    /// add to this one run, if yes.
    pub async fn shell(
        &self,
        command: &str,
        access: Access,
        paths: Vec<PathBuf>,
        ahead: bool,
        cancel: watch::Receiver<bool>,
    ) -> Option<Vec<PathBuf>> {
        let req = PathAsk {
            agent: String::new(),
            tool: crate::tools::shell::Shell::NAME.into(),
            access,
            paths: paths.iter().map(|p| p.display().to_string()).collect(),
            folder: None,
            detail: command.to_string(),
            rerun: !ahead,
            ahead,
            no_always: false,
        };
        let (verdict, roots) = self.ask_paths(req, cancel).await?;
        verdict.allows().then_some(roots)
    }

    /// A delegated child's question, as its parent decides it. A path is
    /// remembered (and written) under the child's definition; an ACP agent's
    /// call under its kind. The child gets the decision back to act on.
    pub async fn decide(&self, req: Request) -> Decision {
        let ask = self.asker();
        match &req {
            Request::Path(p) => {
                if p.paths.iter().any(|x| protected(Path::new(x))) {
                    return Decision::DENY;
                }
                // "Always" writes the path into the child's definition, which
                // must stay inside what the session itself may reach
                // (docs/adr/0014); beyond that the user can only lend it.
                let policy = self.policy();
                let inside = p.paths.iter().all(|x| match p.access {
                    Access::Read => policy.check_read(Path::new(x)).is_ok(),
                    Access::Write => policy.check_write(Path::new(x)).is_ok(),
                });
                let mut p = p.clone();
                p.no_always = !inside;
                let d = ask.ask(Request::Path(p.clone())).await;
                let d = if p.no_always && d.verdict == Verdict::Always {
                    Decision { verdict: Verdict::Session, ..d }
                } else {
                    d
                };
                let roots = Self::granted(&p, d);
                if let Some(error) = self.apply(&p, d, &roots) {
                    self.record(Event::Error { message: error });
                }
                d
            }
            Request::Tool(t) => self.tool(&*ask, t).await,
        }
    }

    async fn tool(&self, ask: &dyn Ask, t: &ToolAsk) -> Decision {
        let key = t.key();
        if self.grants().has_tool(&t.agent, &key) {
            return Decision { verdict: Verdict::Session, folder: false };
        }
        let d = ask.ask(Request::Tool(t.clone())).await;
        if matches!(d.verdict, Verdict::Session | Verdict::Always) {
            self.grants().add_tool(&t.agent, key);
        }
        if d.verdict == Verdict::Always
            && let Some(kind) = &t.kind
            && let Err(e) = persist::allow_kind(&self.paths, &t.agent, kind)
        {
            self.record(Event::Error { message: format!("couldn't save {kind} to config.toml: {e:#}") });
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Answers from a script and remembers what it was asked.
    struct Script {
        answers: Mutex<VecDeque<Decision>>,
        asked: Mutex<Vec<Request>>,
    }

    impl Script {
        fn new(answers: &[Verdict]) -> Arc<Self> {
            let answers = answers.iter().map(|v| Decision { verdict: *v, folder: false }).collect();
            Arc::new(Self { answers: Mutex::new(answers), asked: Mutex::new(vec![]) })
        }
        fn asked(&self) -> Vec<Request> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Ask for Script {
        async fn ask(&self, req: Request) -> Decision {
            self.asked.lock().unwrap().push(req);
            self.answers.lock().unwrap().pop_front().unwrap_or(Decision::DENY)
        }
    }

    /// A policy covering `work`, and a file outside it.
    fn setup(tag: &str) -> (Perms, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("construct-perms-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (work, other) = (root.join("work"), root.join("other"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("f.txt"), "x").unwrap();
        let (work, file) = (work.canonicalize().unwrap(), other.canonicalize().unwrap().join("f.txt"));
        let policy = Policy { read: vec![work.clone()], write: vec![work], ..Default::default() };
        (Perms::new(&Paths { home: root.join("home") }, policy), file, root)
    }

    fn no_cancel() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    #[tokio::test]
    async fn once_passes_that_check_only_and_session_passes_the_rest() {
        let (perms, file, _) = setup("once");
        let ask = Script::new(&[Verdict::Once, Verdict::Session]);
        perms.set_ask(ask.clone());
        assert!(perms.path("read", Access::Read, &file, no_cancel()).await.is_ok());
        assert!(perms.policy().check_read(&file).is_err(), "once is not remembered");
        assert!(perms.path("read", Access::Read, &file, no_cancel()).await.is_ok());
        assert!(perms.path("read", Access::Read, &file, no_cancel()).await.is_ok());
        assert_eq!(ask.asked().len(), 2, "the third check needed no question");
        // Read does not grant write.
        assert!(perms.path("write", Access::Write, &file, no_cancel()).await.is_err());
    }

    #[tokio::test]
    async fn a_decision_nobody_made_is_a_denial_with_no_record() {
        let (perms, file, _) = setup("noask");
        let err = perms.path("read", Access::Read, &file, no_cancel()).await.unwrap_err();
        assert!(err.starts_with("blocked by the sandbox") && !err.contains("declined"), "{err}");
    }

    #[tokio::test]
    async fn interrupting_beats_a_question_nobody_answers() {
        struct Never;
        #[async_trait::async_trait]
        impl Ask for Never {
            async fn ask(&self, _: Request) -> Decision {
                std::future::pending().await
            }
        }
        let (perms, file, _) = setup("cancel");
        perms.set_ask(Arc::new(Never));
        let (tx, rx) = watch::channel(false);
        let pending = perms.path("read", Access::Read, &file, rx);
        tokio::pin!(pending);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending).await.is_err());
        tx.send_replace(true);
        assert!(pending.await.is_err());
    }

    #[tokio::test]
    async fn a_childs_always_is_only_lent_when_it_would_leave_the_sessions_own_reach() {
        let (perms, file, root) = setup("child");
        let inside = root.join("work").canonicalize().unwrap().join("g.txt");
        std::fs::write(&inside, "y").unwrap();
        let ask = Script::new(&[Verdict::Always, Verdict::Always]);
        perms.set_ask(ask.clone());
        let req = |p: &Path| {
            Request::Path(PathAsk {
                agent: "scout".into(),
                tool: "read".into(),
                access: Access::Read,
                paths: vec![p.display().to_string()],
                folder: None,
                detail: String::new(),
                rerun: false,
                ahead: false,
                no_always: false,
            })
        };
        // Beyond the session's reach: the user saw no "always", and a child
        // claiming one gets a session grant, nothing is written.
        let d = perms.decide(req(&file)).await;
        assert_eq!(d.verdict, Verdict::Session);
        assert!(!perms.paths.config().exists());
        assert_eq!(perms.roots_for("scout").read, std::slice::from_ref(&file));
        // Inside it: written to the child's definition.
        let d = perms.decide(req(&inside)).await;
        assert_eq!(d.verdict, Verdict::Always);
        let cfg = std::fs::read_to_string(perms.paths.config()).unwrap();
        assert!(cfg.contains("[agents.scout]") && cfg.contains("allow_read"), "{cfg}");
        let asked = ask.asked();
        let flags: Vec<bool> = asked.iter().map(|r| matches!(r, Request::Path(p) if p.no_always)).collect();
        assert_eq!(flags, [true, false]);
    }

    #[tokio::test]
    async fn a_protected_path_from_a_child_is_refused_without_asking() {
        let (perms, _, _) = setup("prot");
        let ask = Script::new(&[Verdict::Always]);
        perms.set_ask(ask.clone());
        let home = std::env::var("HOME").unwrap();
        let req = Request::Path(PathAsk {
            agent: "scout".into(),
            tool: "read".into(),
            access: Access::Read,
            paths: vec![format!("{home}/.ssh/id_rsa")],
            folder: None,
            detail: String::new(),
            rerun: false,
            ahead: false,
            no_always: false,
        });
        assert_eq!(perms.decide(req).await, Decision::DENY);
        assert!(ask.asked().is_empty());
    }

    #[test]
    fn a_folder_is_only_taken_if_it_is_the_one_that_would_have_been_offered() {
        let (_, file, _) = setup("folder");
        let ask = |folder: &Path| PathAsk {
            agent: String::new(),
            tool: "read".into(),
            access: Access::Read,
            paths: vec![file.display().to_string()],
            folder: Some(folder.display().to_string()),
            detail: String::new(),
            rerun: false,
            ahead: false,
            no_always: false,
        };
        let d = Decision { verdict: Verdict::Session, folder: true };
        let real = file.parent().unwrap();
        assert_eq!(Perms::granted(&ask(real), d), [real.to_path_buf()]);
        // A child that names some other folder gets the path itself.
        assert_eq!(Perms::granted(&ask(Path::new("/")), d), std::slice::from_ref(&file));
    }
}
