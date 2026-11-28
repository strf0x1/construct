//! The system prompt. Kept short on purpose: no skill index (the router puts
//! at most three suggestions on the user turn instead) and no tool essays.
//! The one exception is the sandbox: its limits are stated up front, from the
//! config in force, so the model works within them instead of finding them
//! one failed call at a time.

use crate::config::SandboxConfig;
use crate::memory;
use std::path::Path;

const BASE: &str = "You are construct, a coding agent working in the user's terminal. Use your tools to do what the user asks, then stop.

- Read before you edit. Make the smallest change that does the job and match the surrounding code.
- Prefer read/grep/glob/edit over {shell} for files. Run independent tool calls in parallel.
- Verify your work (build, test, run) when you can, and report failures honestly.
- Be brief. Use GitHub markdown. Refer to code as path:line.
- Don't narrate before tool calls. When done, say what changed and how you verified it in a few sentences.
{skills}- <system-reminder> blocks come from the harness, not the user.";

/// The wire names of the tools the prompt names; the session's builder
/// (`agent`) owns them, so `prompt` does not depend on `tools`. The skill
/// tools are `None` in a build without them (#271), and the lines that
/// would name them are left out.
#[derive(Clone, Copy)]
pub struct ToolNames {
    pub shell: &'static str,
    pub grep: &'static str,
    pub skill_view: Option<&'static str>,
    pub skill_manage: Option<&'static str>,
}

/// The rule that keeps skill files away from file tools, when both skill
/// tools exist to send the model to instead.
fn skills_rule(names: ToolNames) -> String {
    match (names.skill_view, names.skill_manage) {
        (Some(v), Some(m)) => format!(
            "- The skill library is outside the workspace; read and change skills only with the {v} and {m} tools, never file tools or {}.\n",
            names.shell
        ),
        _ => String::new(),
    }
}

/// The fixed part of the prompt, naming the shell tool through its constant
/// and the content-search tool as it is called.
fn base(names: ToolNames, search: Option<&str>) -> String {
    let base = BASE.replace("{skills}", &skills_rule(names)).replace("{shell}", names.shell);
    let named = format!("/{}/", names.grep);
    match search {
        Some(n) if n == names.grep => base,
        Some(n) => base.replace(&named, &format!("/{n}/")),
        None => base.replace(&named, "/"),
    }
}

/// `strategy` is `[learn] strategy`: a learner that keeps its own store
/// (`learn::prompt_block`) adds it here. `search` is what the content-search
/// tool is called this session (`ToolsConfig::search_tool`), or `None` when
/// it does not exist: the prompt never names a tool the model does not have.
pub fn build(
    cwd: &Path,
    memory: &memory::Store,
    skills: &crate::skills::Stack,
    strategy: &str,
    names: ToolNames,
    search: Option<&str>,
    sandbox: &SandboxConfig,
) -> String {
    let mut s = base(names, search);
    s.push_str("\n\n# Environment\n");
    s.push_str(&format!("- cwd: {}\n", cwd.display()));
    s.push_str(&format!("- os: {} {}\n", std::env::consts::OS, std::env::consts::ARCH));
    if cwd.join(".git").exists() {
        s.push_str("- git repo: yes\n");
    }
    s.push_str(&format!("- date: {}\n", today()));
    s.push_str(&sandbox_block(sandbox));
    for name in ["AGENTS.md", "CLAUDE.md"] {
        if let Ok(text) = std::fs::read_to_string(cwd.join(name)) {
            s.push_str(&format!("\n# Project instructions ({name})\n{}\n", crate::text::truncate(text.trim(), 8000)));
            break;
        }
    }
    s.push_str(&memory.prompt_block());
    s.push_str(&crate::learn::prompt_block(strategy, memory));
    if let (Some(kit), Some(view)) = (skills.kit(), names.skill_view) {
        let name = kit.name();
        let kit = kit.list();
        if !kit.is_empty() {
            s.push_str(&format!("\n# Skills for this project ({name}) — load with the {view} tool when one applies\n"));
            for k in kit {
                s.push_str(&format!("- {}: {}\n", k.name, crate::text::truncate(&k.description, 200)));
            }
        }
    }
    s
}

/// What the tools can and cannot reach, as the model needs to know it. It
/// mirrors `sandbox::Policy::new`, which never blocks the network itself.
fn sandbox_block(cfg: &SandboxConfig) -> String {
    let secrets = "- Credentials and secrets are removed from the tools' environment.\n";
    let network = "- The network is not blocked by construct, but the machine may be offline. If a fetch fails, don't retry it; work from what is on disk.\n";
    if cfg.mode == "off" {
        return format!("\n# Sandbox\n- None: tools run with the user's own permissions.\n{secrets}{network}");
    }
    let mut s = String::from("\n# Sandbox\nTools run in a sandbox; work within it rather than around it.\n");
    if cfg.mode == "read-only" {
        s.push_str("- Read-only: the working directory, system paths and installed toolchains.\n");
        s.push_str("- Writable: only $TMPDIR, for scratch files (/tmp is not writable).\n");
    } else {
        s.push_str("- Read and write: the working directory, and $TMPDIR for scratch files (/tmp is not writable).\n");
        s.push_str("- Read-only: system paths and installed toolchains.\n");
    }
    if !cfg.allow_read.is_empty() {
        s.push_str(&format!("- Also readable: {}\n", cfg.allow_read.join(", ")));
    }
    if !cfg.allow_write.is_empty() {
        s.push_str(&format!("- Also writable: {}\n", cfg.allow_write.join(", ")));
    }
    s.push_str("- Everything else on disk is hidden, so a search outside these finds nothing. Search the workspace; if something isn't there, ask the user where it is.\n");
    s.push_str(
        "- A call that goes past these limits asks the user when someone can answer, and is denied otherwise.\n",
    );
    s.push_str(secrets);
    s.push_str(network);
    s
}

fn today() -> String {
    let secs = (crate::journal::now_ms() / 1000) as i64;
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
/// The user message for `/<skill> text`: the skill the user picked, then
/// what they asked. With no `text` the skill itself is the request.
pub fn invoked_skill(name: &str, body: &str, text: &str) -> String {
    let ask = if text.trim().is_empty() {
        "The user gave no further request; follow the skill, and ask what they want if it needs input.".to_string()
    } else {
        text.to_string()
    };
    format!("<skill name=\"{name}\" invoked-by=\"user\">\n{}\n</skill>\n\n{ask}", body.trim_end())
}

/// `None` too in a build with no skill tool (`view`) to load one with.
pub fn skill_reminder(skills: &[(String, f32)], all: &[crate::skills::Skill], view: Option<&str>) -> Option<String> {
    let view = view?;
    if skills.is_empty() {
        return None;
    }
    let lines: Vec<String> = skills
        .iter()
        .filter_map(|(n, _)| all.iter().find(|s| &s.name == n))
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<system-reminder>\nSkills that may fit this request (load with the {view} tool if one does; ignore if not):\n{}\n</system-reminder>",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    const NAMES: ToolNames = ToolNames {
        shell: Shell::NAME,
        grep: Grep::NAME,
        skill_view: Some(crate::skills::VIEW_TOOL),
        skill_manage: Some(crate::skills::MANAGE_TOOL),
    };
    use super::*;
    use crate::tools::{grep::Grep, shell::Shell};

    fn sandbox(mode: &str) -> SandboxConfig {
        SandboxConfig { mode: mode.into(), ..SandboxConfig::default() }
    }

    #[test]
    fn the_sandbox_block_states_the_limits_in_force() {
        let ws = sandbox_block(&sandbox("workspace"));
        assert!(ws.contains("Read and write: the working directory"), "{ws}");
        assert!(ws.contains("/tmp is not writable") && ws.contains("may be offline"), "{ws}");
        let ro = sandbox_block(&sandbox("read-only"));
        assert!(ro.contains("Writable: only $TMPDIR") && !ro.contains("Read and write"), "{ro}");
        let off = sandbox_block(&sandbox("off"));
        assert!(off.contains("None: tools run with the user's own permissions") && !off.contains("hidden"), "{off}");
        let extra = SandboxConfig { allow_read: vec!["~/notes".into()], ..sandbox("workspace") };
        assert!(sandbox_block(&extra).contains("Also readable: ~/notes"));
    }

    fn tools_line(search: Option<&str>) -> String {
        base(NAMES, search)
            .lines()
            .find(|l| l.contains(&format!("over {} for files", Shell::NAME)))
            .unwrap()
            .to_string()
    }

    #[test]
    fn the_prompt_names_the_search_tool_as_it_is_called() {
        // The golden request snapshots pin the whole default prompt.
        assert!(!base(NAMES, Some("grep")).contains("{shell}") && !base(NAMES, Some("grep")).contains("{skills}"));
        assert!(tools_line(Some("grep")).contains("read/grep/glob/edit"));
        assert!(tools_line(Some("search")).contains("read/search/glob/edit"));
        // No search tool: the prompt must not offer one.
        assert!(tools_line(None).contains("read/glob/edit") && !tools_line(None).contains("grep"));
    }

    #[test]
    fn the_prompt_names_skill_tools_only_when_the_build_has_them() {
        let rule = "read and change skills only with the skill and skill_manage tools, never file tools or bash.";
        assert!(base(NAMES, Some("grep")).contains(rule));
        let bare = ToolNames { skill_view: None, skill_manage: None, ..NAMES };
        assert!(!base(bare, Some("grep")).contains("skill"), "{}", base(bare, Some("grep")));
        let all = [crate::skills::Skill { name: "x".into(), description: "d".into(), dir: Default::default() }];
        let picked = [("x".to_string(), 0.9)];
        assert!(skill_reminder(&picked, &all, Some("skill")).unwrap().contains("load with the skill tool"));
        assert!(skill_reminder(&picked, &all, None).is_none());
    }
}
