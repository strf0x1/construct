//! Approvals for frozen developer operations. Session grants compare the full
//! request, including executable, working directory and input, rather than a
//! command name or an operation kind. They never widen the shell sandbox.

use super::perms::raced;
use super::{Perms, Request, Verdict};
use crate::event::Event;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationAsk {
    #[serde(default)]
    pub agent: String,
    pub operation: String,
    pub scope: String,
    /// The complete frozen execution plan, without credentials.
    pub detail: String,
}

impl Perms {
    /// Authorize this exact execution plan. Reusable grants are session-only;
    /// a different destination, input or executable requires another answer.
    pub async fn operation(&self, req: OperationAsk, cancel: watch::Receiver<bool>) -> bool {
        let granted = self.operations.lock().unwrap_or_else(|e| e.into_inner()).contains(&req);
        let verdict = if *cancel.borrow() {
            Verdict::Cancel
        } else if !req.agent.is_empty() {
            // Children do not inherit the host runner or its credentials.
            Verdict::Deny
        } else if granted {
            Verdict::Session
        } else {
            let ask = self.asker();
            if ask.interactive() {
                raced(ask.ask(Request::Operation(req.clone())), cancel).await.verdict
            } else {
                Verdict::Deny
            }
        };
        let verdict = if verdict == Verdict::Always { Verdict::Deny } else { verdict };
        if verdict == Verdict::Session && !granted {
            self.operations.lock().unwrap_or_else(|e| e.into_inner()).push(req.clone());
        }
        self.record(Event::Operation {
            operation: req.operation,
            scope: req.scope,
            detail: req.detail,
            decision: verdict.word().into(),
        });
        verdict.allows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;
    use crate::permission::{Ask, Decision};
    use crate::sandbox::Policy;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    struct Answers {
        values: Mutex<VecDeque<Verdict>>,
        requests: Mutex<Vec<Request>>,
    }

    #[async_trait::async_trait]
    impl Ask for Answers {
        async fn ask(&self, request: Request) -> Decision {
            self.requests.lock().unwrap().push(request);
            Decision { verdict: self.values.lock().unwrap().pop_front().unwrap_or(Verdict::Deny), folder: false }
        }
    }

    fn setup(values: &[Verdict]) -> (Perms, Arc<Answers>) {
        let perms = Perms::new(&Paths { home: std::env::temp_dir() }, Policy::default());
        let answers =
            Arc::new(Answers { values: Mutex::new(values.iter().copied().collect()), requests: Mutex::new(vec![]) });
        perms.set_ask(answers.clone());
        (perms, answers)
    }

    fn request() -> OperationAsk {
        OperationAsk {
            agent: String::new(),
            operation: "issue_create".into(),
            scope: "github.com/owner/repo".into(),
            detail: "executable=/usr/bin/gh cwd=/private/tmp/construct input={title: bug, body: reproduction}".into(),
        }
    }

    fn cancel() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    #[tokio::test]
    async fn session_grant_only_reuses_the_complete_execution_plan() {
        let (perms, answers) = setup(&[Verdict::Session]);
        let req = request();
        assert!(perms.operation(req.clone(), cancel()).await);
        assert!(perms.operation(req.clone(), cancel()).await);
        let mut changed = req.clone();
        changed.detail.push_str(" substituted body");
        assert!(!perms.operation(changed, cancel()).await);
        let mut changed = req.clone();
        changed.scope = "github.com/owner/other".into();
        assert!(!perms.operation(changed, cancel()).await);
        let mut changed = req;
        changed.operation = "push".into();
        assert!(!perms.operation(changed, cancel()).await);
        assert_eq!(answers.requests.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn once_denial_and_unsupported_always_do_not_grant_future_calls() {
        let (perms, answers) = setup(&[Verdict::Once, Verdict::Deny, Verdict::Always]);
        assert!(perms.operation(request(), cancel()).await);
        assert!(!perms.operation(request(), cancel()).await);
        assert!(!perms.operation(request(), cancel()).await);
        assert!(!perms.operation(request(), cancel()).await);
        assert_eq!(answers.requests.lock().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn cancellation_and_children_cannot_use_the_host_grant() {
        let (perms, answers) = setup(&[Verdict::Session]);
        assert!(perms.operation(request(), cancel()).await);
        let cancelled = watch::channel(true).1;
        assert!(!perms.operation(request(), cancelled).await);
        let mut child = request();
        child.agent = "child".into();
        assert!(!perms.operation(child.clone(), cancel()).await);
        assert_eq!(perms.decide(Request::Operation(child)).await, Decision::DENY);
        assert_eq!(answers.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn headless_denies() {
        let perms = Perms::new(&Paths { home: std::env::temp_dir() }, Policy::default());
        assert!(!perms.operation(request(), cancel()).await);
    }
}
