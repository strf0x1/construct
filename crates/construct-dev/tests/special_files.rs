//! File reads reject FIFOs and devices without stalling the agent. The read
//! runs through the real binary, with a process deadline so a blocked open
//! fails the test instead of hanging the suite.

use crate::common::*;
use serde_json::json;
use std::os::unix::fs::symlink;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run_bounded(env: &Env) {
    let mut command = Command::new(construct());
    command
        .args(["run", "--no-learn", "read the paths"])
        .current_dir(&env.work)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in env.env() {
        if value.is_empty() {
            command.env_remove(key);
        } else {
            command.env(key, value);
        }
    }
    let mut child = Running(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_millis(budget(8000));
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "file reads failed the turn: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "file read stalled instead of rejecting a special file");
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn reads_reject_special_files_and_the_agent_continues() {
    for mode in ["workspace", "off"] {
        let env = Env::new(
            &format!("special-files-{mode}"),
            json!({ "steps": [
                { "call": "read", "args": { "path": "pipe", "limit": 1 } },
                { "call": "read", "args": { "path": "pipe-link" } },
                { "call": "read", "args": { "path": "/dev/null" } },
                { "call": "read", "args": { "path": "notes.txt" } },
                { "call": "read", "args": { "path": "." } },
                { "text": "finished reading" }
            ] }),
        );
        env.config(&format!("[sandbox]\nmode = \"{mode}\"\n"));
        let pipe = env.work.join("pipe");
        assert!(Command::new("mkfifo").arg(&pipe).status().unwrap().success(), "creating {}", pipe.display());
        symlink("pipe", env.work.join("pipe-link")).unwrap();
        std::fs::write(env.work.join("notes.txt"), "ordinary text\n").unwrap();

        run_bounded(&env);

        let rows = env.journal();
        let ends: Vec<_> = rows.iter().filter(|r| r.ty == "tool_end").map(|r| &r.payload).collect();
        assert_eq!(ends.len(), 5, "{mode}: every read must complete");
        for (end, path) in ends.iter().take(3).zip(["pipe", "pipe", "/dev/null"]) {
            assert_eq!(end["ok"], false, "{mode}: {end}");
            let output = end["output"].as_str().unwrap();
            assert!(output.contains(path) && output.contains("unsupported file type"), "{output}");
        }
        for (end, expected) in ends.iter().skip(3).zip(["ordinary text", "notes.txt"]) {
            assert_eq!(end["ok"], true, "{mode}: {end}");
            assert!(end["output"].as_str().unwrap().contains(expected), "{end}");
        }
        // Check the actual tool messages sent back, not the scripted answer.
        let requests = env.requests();
        assert_eq!(requests.len(), 6, "{mode}: the agent must continue after refusals");
        for request in requests.iter().skip(1).take(3) {
            assert!(last_tool_result(request).contains("unsupported file type"));
        }
    }
}
