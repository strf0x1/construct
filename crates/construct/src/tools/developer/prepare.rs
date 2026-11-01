//! Prepare immutable developer operations before asking. Execution consumes
//! the prepared inputs, so approval cannot be transferred to another payload.

use super::{git, github, runner};
use crate::permission::OperationAsk;
use crate::tools::{Ctx, Output, ToolError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;

pub struct Prepared {
    pub request: OperationAsk,
    pub(super) executable: runner::Executable,
    pub(super) cwd: PathBuf,
    pub(super) operation: Body,
}

pub(super) enum Body {
    Github { endpoint: String, input: Vec<u8> },
    Commit(git::Commit),
    Push(git::Push),
}

pub fn required<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    args.get(name).and_then(Value::as_str).filter(|s| !s.is_empty() && !s.contains('\0')).ok_or_else(|| {
        ToolError::new("invalid_argument", format!("{name} must be a nonempty string without NUL bytes"))
    })
}

pub async fn prepare(ctx: &Ctx, args: Value) -> Result<Prepared, ToolError> {
    let operation = required(&args, "operation")?;
    let allowed: &[&str] = match operation {
        "issue_create" => &["operation", "repo", "title", "body"],
        "pr_create" => &["operation", "repo", "base", "head", "title", "body"],
        "commit" => &["operation", "workdir", "message"],
        "push" => &["operation", "workdir", "remote", "branch"],
        _ => &[],
    };
    if args.as_object().is_none_or(|map| map.keys().any(|key| !allowed.contains(&key.as_str()))) {
        return Err("unexpected developer-operation argument".into());
    }
    let mut prepared = match operation {
        "issue_create" | "pr_create" => github::prepare(ctx, &args),
        "commit" | "push" => git::prepare(ctx, &args).await,
        _ => Err(ToolError::new("invalid_argument", "operation must be issue_create, commit, push, or pr_create")),
    }?;
    let hash = prepared.fingerprint()?;
    prepared.request.detail.push_str(&format!("\nExecution fingerprint: {hash}"));
    Ok(prepared)
}

impl Prepared {
    fn fingerprint(&self) -> Result<String, ToolError> {
        let payload = match &self.operation {
            Body::Github { endpoint, input } => {
                serde_json::to_vec(&(endpoint, input)).map_err(|_| "cannot encode operation")?
            }
            other => git::binding(other)?,
        };
        let encoded = serde_json::to_vec(&(
            &self.request.operation,
            &self.request.scope,
            self.executable.identity(),
            &self.cwd,
            payload,
        ))
        .map_err(|_| "cannot encode operation binding")?;
        Ok(digest(&encoded))
    }

    pub async fn execute(self, ctx: &Ctx) -> Result<Output, ToolError> {
        self.executable.check()?;
        let remote = !matches!(&self.operation, Body::Commit(_));
        let ledger = remote.then(|| Ledger::begin(ctx, &self)).transpose()?;
        if let Some(Ledger { previous: Some(text), .. }) = &ledger {
            return Ok(Output::plain(format!("Already completed: {text}"))
                .with_data(json!({"operation": self.request.operation, "already_completed": true})));
        }
        let outcome = self.perform(ctx).await;
        match outcome {
            Ok(text) => {
                if let Some(ledger) = ledger {
                    ledger.success(&text)?;
                }
                Ok(Output::plain(text)
                    .with_data(json!({"operation": self.request.operation, "scope": self.request.scope})))
            }
            Err(error) => {
                if matches!(error.code, "spawn" | "changed" | "interrupted")
                    && let Some(ledger) = ledger
                {
                    let _ = std::fs::remove_file(ledger.pending);
                }
                Err(error)
            }
        }
    }

    async fn perform(&self, ctx: &Ctx) -> Result<String, ToolError> {
        match &self.operation {
            Body::Github { endpoint, input } => {
                let bytes = runner::run(
                    ctx,
                    &self.executable,
                    &self.cwd,
                    &runner::args(&["api", "--hostname", "github.com", "--method", "POST", endpoint, "--input", "-"]),
                    input,
                    &[],
                )
                .await?;
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                    ToolError::new(
                        "unknown",
                        "GitHub write returned an unreadable response; reconcile before repeating",
                    )
                })?;
                let url = value
                    .get("html_url")
                    .and_then(Value::as_str)
                    .filter(|s| s.starts_with("https://github.com/"))
                    .ok_or_else(|| {
                        ToolError::new("unknown", "GitHub write returned no result URL; reconcile before repeating")
                    })?;
                Ok(format!("Created {url}"))
            }
            Body::Commit(commit) => git::commit(ctx, &self.executable, &self.cwd, commit).await,
            Body::Push(push) => git::push(ctx, &self.executable, &self.cwd, push).await,
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct Ledger {
    pending: PathBuf,
    result: PathBuf,
    previous: Option<String>,
}

impl Ledger {
    fn begin(ctx: &Ctx, operation: &Prepared) -> Result<Self, ToolError> {
        use std::os::unix::fs::OpenOptionsExt;
        let root = runner::private_directory(ctx, "developer-operations")?;
        let pending = root.join(format!(
            "{}.pending",
            digest(format!("{}\0{}", operation.request.operation, operation.request.scope).as_bytes())
        ));
        let result = root.join(format!("{}.done", operation.fingerprint()?));
        let previous = std::fs::read_to_string(&result).ok();
        if previous.is_none() {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&pending)
                .map_err(|_| ToolError::new("unknown", "A host write in this scope has an unresolved result. Reconcile it before retrying; the pending record is in CONSTRUCT_HOME/developer-operations."))?;
            file.write_all(operation.fingerprint()?.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|_| "cannot record operation before execution")?;
        }
        Ok(Self { pending, result, previous })
    }

    fn success(self, text: &str) -> Result<(), ToolError> {
        use std::os::unix::fs::OpenOptionsExt;
        let temporary = self.result.with_extension(format!("done-{}", crate::skills::now_nanos()));
        let mut file =
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary).map_err(|_| {
                ToolError::new(
                    "unknown",
                    "Host write succeeded but its result could not be recorded; reconcile before repeating",
                )
            })?;
        file.write_all(text.as_bytes()).and_then(|_| file.sync_all()).map_err(|_| {
            ToolError::new(
                "unknown",
                "Host write succeeded but its result could not be recorded; reconcile before repeating",
            )
        })?;
        std::fs::rename(temporary, &self.result).map_err(|_| {
            ToolError::new(
                "unknown",
                "Host write succeeded but its result could not be recorded; reconcile before repeating",
            )
        })?;
        std::fs::remove_file(&self.pending)
            .map_err(|_| "host write succeeded; its pending marker could not be removed")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MemoryConfig, Paths, SandboxConfig};
    use crate::memory::Store;
    use crate::sandbox::Policy;
    use std::os::unix::fs::PermissionsExt;

    fn context(name: &str) -> (Ctx, PathBuf) {
        let root = std::env::temp_dir().join(format!("construct-developer-{name}-{}", crate::skills::now_nanos()));
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let paths = Paths { home: root.join("home") };
        let store = Store::new(&paths, &MemoryConfig::default());
        let policy = Policy::new(&SandboxConfig::default(), &work, &paths);
        let ctx = Ctx::new(work, paths, store, tokio::sync::watch::channel(false).1, None, policy);
        (ctx, root)
    }

    fn prepared(ctx: &Ctx, root: &std::path::Path, script: &str, input: Value) -> Prepared {
        let file = root.join("fake-gh");
        std::fs::write(&file, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        Prepared {
            request: OperationAsk {
                agent: String::new(),
                operation: "issue_create".into(),
                scope: "github.com/o/r".into(),
                detail: "review".into(),
            },
            executable: runner::Executable::fake(file),
            cwd: runner::private_root(ctx).unwrap(),
            operation: Body::Github { endpoint: "repos/o/r/issues".into(), input: serde_json::to_vec(&input).unwrap() },
        }
    }

    #[tokio::test]
    async fn exact_argv_and_json_are_executed_once_and_success_is_not_replayed() {
        let (ctx, root) = context("execute");
        let script = format!(
            "printf '%s\\n' \"$@\" > '{}/args'\ncat > '{}/input'\nprintf '%s' '{{\"html_url\":\"https://github.com/o/r/issues/1\"}}'",
            root.display(),
            root.display()
        );
        let input = json!({"title":"literal $(touch bad)","body":"body\nwith newline"});
        let plan = prepared(&ctx, &root, &script, input.clone());
        let output = plan.execute(&ctx).await.unwrap();
        assert_eq!(output.text, "Created https://github.com/o/r/issues/1");
        assert_eq!(
            std::fs::read_to_string(root.join("args")).unwrap(),
            "api\n--hostname\ngithub.com\n--method\nPOST\nrepos/o/r/issues\n--input\n-\n"
        );
        assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(root.join("input")).unwrap()).unwrap(), input);
        std::fs::remove_file(root.join("args")).unwrap();
        let output = prepared(&ctx, &root, &script, input).execute(&ctx).await.unwrap();
        assert!(output.text.starts_with("Already completed:"));
        assert!(!root.join("args").exists());
        assert!(!ctx.cwd.join("bad").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn lost_response_blocks_other_writes_in_scope_even_with_new_context() {
        let (ctx, root) = context("ambiguous");
        let input = json!({"title":"a","body":"b"});
        let plan = prepared(&ctx, &root, "printf invalid-json", input.clone());
        assert_eq!(plan.execute(&ctx).await.err().unwrap().code, "unknown");
        let ctx = Ctx::new(
            ctx.cwd.clone(),
            ctx.paths.clone(),
            Store::new(&ctx.paths, &MemoryConfig::default()),
            tokio::sync::watch::channel(false).1,
            None,
            ctx.sandbox.clone(),
        );
        let plan = prepared(&ctx, &root, "touch should-not-run", json!({"title":"different","body":"b"}));
        assert_eq!(plan.execute(&ctx).await.err().unwrap().code, "unknown");
        assert!(!runner::private_root(&ctx).unwrap().join("should-not-run").exists());
        let plan = prepared(&ctx, &root, "touch should-not-run", input);
        assert_eq!(plan.execute(&ctx).await.err().unwrap().code, "unknown");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn repository_casing_cannot_bypass_an_unresolved_github_write() {
        let (ctx, root) = context("repository-case");
        let make = |spelling: &str, script: &str| {
            let repo = github::repository(spelling).unwrap();
            let mut plan = prepared(&ctx, &root, script, json!({"title":"bug", "body":"details"}));
            plan.request.scope = format!("github.com/{repo}");
            plan.operation = Body::Github {
                endpoint: format!("repos/{repo}/issues"),
                input: serde_json::to_vec(&json!({"title":"bug", "body":"details"})).unwrap(),
            };
            plan
        };
        assert_eq!(make("owner/repo", "printf invalid-json").execute(&ctx).await.err().unwrap().code, "unknown");
        let repeated = make("OWNER/REPO", "touch duplicate-write");
        assert_eq!(repeated.execute(&ctx).await.err().unwrap().code, "unknown");
        assert!(!runner::private_root(&ctx).unwrap().join("duplicate-write").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn executable_replacement_is_refused_before_any_write() {
        let (ctx, root) = context("replacement");
        let plan = prepared(&ctx, &root, "touch should-not-run", json!({"title":"a","body":"b"}));
        std::fs::write(root.join("fake-gh"), "#!/bin/sh\ntouch substituted\n").unwrap();
        assert_eq!(plan.execute(&ctx).await.err().unwrap().code, "changed");
        assert!(!runner::private_root(&ctx).unwrap().join("substituted").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn pull_request_creation_posts_explicit_branches_and_never_pushes() {
        let (ctx, root) = context("pull-request");
        let script = format!(
            "printf '%s\\n' \"$@\" > '{}/args'\ncat > '{}/input'\nprintf '%s' '{{\"html_url\":\"https://github.com/o/r/pull/7\"}}'",
            root.display(),
            root.display()
        );
        let input = json!({"title":"Review change", "body":"Full description", "base":"main", "head":"o:feature"});
        let mut plan = prepared(&ctx, &root, &script, input.clone());
        plan.request.operation = "pr_create".into();
        plan.operation =
            Body::Github { endpoint: "repos/o/r/pulls".into(), input: serde_json::to_vec(&input).unwrap() };
        assert_eq!(plan.execute(&ctx).await.unwrap().text, "Created https://github.com/o/r/pull/7");
        assert_eq!(
            std::fs::read_to_string(root.join("args")).unwrap(),
            "api\n--hostname\ngithub.com\n--method\nPOST\nrepos/o/r/pulls\n--input\n-\n"
        );
        assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(root.join("input")).unwrap()).unwrap(), input);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn authentication_failure_is_distinct_and_does_not_expose_host_output() {
        let (ctx, root) = context("authentication");
        let input = json!({"title":"a","body":"b"});
        let plan =
            prepared(&ctx, &root, "printf 'authentication failed token=secret-value' >&2; exit 1", input.clone());
        let error = plan.execute(&ctx).await.err().unwrap();
        assert_eq!(error.code, "authentication");
        assert!(error.message.contains("outside the tool sandbox"));
        assert!(!error.message.contains("secret-value"));
        let plan = prepared(&ctx, &root, "printf '{\"html_url\":\"https://github.com/o/r/issues/2\"}'", input);
        assert_eq!(plan.execute(&ctx).await.err().unwrap().code, "unknown");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn fingerprint_distinguishes_payloads_with_identical_review_delimiters() {
        let (ctx, root) = context("binding");
        let first = prepared(&ctx, &root, "true", json!({"title":"x\nBody:\ny","body":"z"}));
        let second = prepared(&ctx, &root, "true", json!({"title":"x","body":"y\nBody:\nz"}));
        assert_eq!(first.request.detail, second.request.detail);
        assert_ne!(first.fingerprint().unwrap(), second.fingerprint().unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
