//! MCP write backpressure through the real agent and a sandboxed server.
//! A process deadline catches a blocked pipe without hanging the test suite;
//! a second call proves that a damaged connection is replaced.

use crate::common::*;
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_blocked_request_write_times_out_and_the_next_call_restarts_the_server() {
    // Exceed pipe buffers without exceeding the mock's 2 MiB request limit
    // when the assistant's arguments are echoed on the next model request.
    let payload = "x".repeat(1536 * 1024);
    let env = Env::new(
        "mcp-blocked-write",
        json!({ "steps": [
            { "call": "mcp__blocked__echo", "args": { "text": payload } },
            { "call": "mcp__blocked__echo", "args": { "text": "back after write timeout" } },
            { "text": "done" }
        ] }),
    );
    env.config(&format!(
        "context_window = 4000000\n[router]\nmode = \"local\"\n[mcp.blocked]\ncommand = \"{}\"\nargs = [\"mcp-fake\", \"--stall-stdin-once\", \"stalled\"]\ntimeout = 1\n",
        dev_bin().display()
    ));
    let mut command = Command::new(construct());
    command
        .args(["run", "--no-learn", "use blocked"])
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
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            let errors: Vec<_> = env.journal().into_iter().filter(|r| r.ty == "error").map(|r| r.payload).collect();
            assert!(status.success(), "MCP timeout/recovery failed the turn: {status}; {errors:?}");
            break;
        }
        assert!(Instant::now() < deadline, "MCP request write ignored its 1s timeout");
        std::thread::sleep(Duration::from_millis(15));
    }

    assert!(env.work.join("stalled").exists(), "the server never reached the blocked-stdin state");
    let rows = env.journal();
    let ends: Vec<_> = rows.iter().filter(|r| r.ty == "tool_end").map(|r| &r.payload).collect();
    assert_eq!(ends.len(), 2, "both the blocked call and its replacement must finish");
    assert_eq!(ends[0]["ok"], false);
    let error = ends[0]["output"].as_str().unwrap();
    assert!(error.contains("write") && error.contains("timed out after 1s"), "{error}");
    assert_eq!(ends[1]["ok"], true, "{}", ends[1]);
    assert_eq!(ends[1]["output"], "back after write timeout");
    let requests = env.requests();
    assert_eq!(requests.len(), 3);
    assert!(last_tool_result(&requests[1]).contains("timed out after 1s"));
    assert_eq!(last_tool_result(&requests[2]), "back after write timeout");
}
