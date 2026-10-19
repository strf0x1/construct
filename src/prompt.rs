//! The system prompt. Kept short on purpose: no skill index (the router puts
//! at most three suggestions on the user turn instead) and no tool essays.

use crate::memory;
use std::path::Path;

const BASE: &str = "You are construct, a coding agent working in the user's terminal. Use your tools to do what the user asks, then stop.

- Read before you edit. Make the smallest change that does the job and match the surrounding code.
- Prefer read/grep/glob/edit over shell for files. Run independent tool calls in parallel.
- Verify your work (build, test, run) when you can, and report failures honestly.
- Be brief. Use GitHub markdown. Refer to code as path:line.
- <system-reminder> blocks come from the harness, not the user.";

pub fn build(cwd: &Path, memory: &memory::Store) -> String {
    let mut s = String::from(BASE);
    s.push_str("\n\n# Environment\n");
    s.push_str(&format!("- cwd: {}\n", cwd.display()));
    s.push_str(&format!("- os: {} {}\n", std::env::consts::OS, std::env::consts::ARCH));
    if cwd.join(".git").exists() {
        s.push_str("- git repo: yes\n");
    }
    s.push_str(&format!("- date: {}\n", today()));
    for name in ["AGENTS.md", "CLAUDE.md"] {
        if let Ok(text) = std::fs::read_to_string(cwd.join(name)) {
            s.push_str(&format!("\n# Project instructions ({name})\n{}\n", crate::llm::truncate(text.trim(), 8000)));
            break;
        }
    }
    s.push_str(&memory.prompt_block());
    s
}

fn today() -> String {
    let secs = (crate::session::now_ms() / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The reminder appended to a user turn when the router suggests skills.
pub fn skill_reminder(skills: &[(String, f32)], lib: &crate::skills::Library) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let lines: Vec<String> = skills
        .iter()
        .filter_map(|(n, _)| lib.get(n))
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<system-reminder>\nSkills that may fit this request (load with the skill tool if one does; ignore if not):\n{}\n</system-reminder>",
        lines.join("\n")
    ))
}
