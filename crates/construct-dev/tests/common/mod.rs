//! Shared harness for the end-to-end tests: build the real binary once, give
//! each test a fresh home and its own scripted model endpoint, and read back
//! what the harness sent.
#![allow(dead_code)] // each test binary uses a different subset

use construct_dev::serve;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Once;

/// The workspace root: this crate lives at `crates/construct-dev`.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

/// A timing limit in ms, doubled under `CONSTRUCT_E2E_SLOW=1` (set in CI).
/// These are smoke limits that catch a hang; real timings are the bench's job.
pub fn budget(ms: u64) -> u64 {
    if std::env::var_os("CONSTRUCT_E2E_SLOW").is_some_and(|v| v == "1") { ms * 2 } else { ms }
}

pub fn construct() -> PathBuf {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let ok = Command::new(cargo).args(["build", "-q", "-p", "construct"]).current_dir(root()).status().unwrap();
        assert!(ok.success(), "building construct failed");
    });
    root().join("target/debug/construct")
}

pub fn dev_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_construct-dev"))
}

pub struct Env {
    pub dir: PathBuf,
    pub home: PathBuf,
    pub work: PathBuf,
    pub port: u16,
    _rt: tokio::runtime::Runtime,
}

/// One journal row as stored, payload parsed.
#[derive(Debug, Clone)]
pub struct Row {
    pub seq: i64,
    pub session: String,
    pub ty: String,
    pub v: i64,
    pub at_ms: i64,
    pub payload: Value,
}

impl Env {
    /// Where `Env::new(name, ..)` puts its files, for paths a script needs up front.
    pub fn dir_for(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("construct-e2e-{name}-{}", std::process::id()))
    }

    pub fn new(name: &str, steps: Value) -> Self {
        Self::with_window(name, steps, None)
    }

    /// An `Env` whose model has a `window`-token context (see `serve::Config::window`).
    pub fn with_window(name: &str, steps: Value, window: Option<u64>) -> Self {
        let dir = Self::dir_for(name);
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let cfg = serve::Config {
            script: serde_json::from_value(steps["steps"].clone()).unwrap(),
            repeat_call: None,
            side: serde_json::from_value(steps["side"].clone()).unwrap_or_default(),
            upstream: None,
            key: None,
            log: Some(dir.join("log.jsonl")),
            dump_dir: Some(dir.join("dump")),
            chunk_ms: 1,
            window,
        };
        let (port, _) = rt.block_on(serve::serve(cfg, 0)).unwrap();
        Self { dir, home, work, port, _rt: rt }
    }

    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("CONSTRUCT_HOME".into(), self.home.display().to_string()),
            ("CONSTRUCT_BASE_URL".into(), format!("http://127.0.0.1:{}/v1", self.port)),
            ("OPENAI_API_KEY".into(), "test".into()),
            ("CONSTRUCT_LEARN_DEBOUNCE_MS".into(), "0".into()),
            // Empty means unset: the developer's shell must not change what the tests see.
            ("TYPESAFE_API_KEY".into(), String::new()),
            ("CONSTRUCT_MODEL".into(), String::new()),
            ("CONSTRUCT_ROUTER".into(), String::new()),
            ("CONSTRUCT_SANDBOX".into(), String::new()),
            ("CONSTRUCT_TOOLS_EDITOR".into(), String::new()),
            ("CONSTRUCT_TRACE".into(), String::new()),
        ]
    }

    pub fn config(&self, toml: &str) {
        std::fs::create_dir_all(&self.home).unwrap();
        std::fs::write(self.home.join("config.toml"), toml).unwrap();
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.run_with(&[], args)
    }

    pub fn run_with(&self, extra: &[(&str, &str)], args: &[&str]) -> Output {
        self.run_in(&self.work, extra, args)
    }

    pub fn run_in(&self, dir: &Path, extra: &[(&str, &str)], args: &[&str]) -> Output {
        let mut c = Command::new(construct());
        c.args(args).current_dir(dir);
        for (k, v) in self.env() {
            if v.is_empty() {
                c.env_remove(k);
            } else {
                c.env(k, v);
            }
        }
        c.envs(extra.iter().copied());
        c.output().unwrap()
    }

    /// Every request body the harness sent, in order.
    pub fn requests(&self) -> Vec<Value> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.dir.join("dump"))
            .map(|r| r.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        files.iter().map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()).collect()
    }

    /// The journal file for sessions run in `dir`.
    pub fn journal_path(&self, dir: &Path) -> PathBuf {
        construct::journal::path(&construct::config::Paths { home: self.home.clone() }, dir)
    }

    /// Every row of the work dir's journal, in seq order, read with plain SQL.
    pub fn journal(&self) -> Vec<Row> {
        self.journal_in(&self.work)
    }

    pub fn journal_in(&self, dir: &Path) -> Vec<Row> {
        let path = self.journal_path(dir);
        if !path.exists() {
            return vec![];
        }
        let db = rusqlite::Connection::open(path).unwrap();
        let mut st = db.prepare("SELECT seq, session_id, type, v, at_ms, payload FROM events ORDER BY seq").unwrap();
        st.query_map([], |r| {
            Ok(Row {
                seq: r.get(0)?,
                session: r.get(1)?,
                ty: r.get(2)?,
                v: r.get(3)?,
                at_ms: r.get(4)?,
                payload: serde_json::from_str(&r.get::<_, String>(5)?).unwrap(),
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Session ids in the work dir's journal, in the order they started.
    pub fn sessions(&self) -> Vec<String> {
        self.journal().into_iter().filter(|r| r.ty == "head").map(|r| r.session).collect()
    }
}

pub fn tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|t| t.iter().map(|x| x["function"]["name"].as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

pub fn last_tool_result(req: &Value) -> String {
    req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

pub fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
