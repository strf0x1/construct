# Proposal 4 — Tools as a trait, config as the profile

Status: done, for issue #1. Steps 1–3 landed in #23, step 4 in #24 (and
hash-only anchors in #25), step 5 with this status line, step 6 in #28 (ADR
0002; the stale-duplicate follow-ups are ADRs 0007 and 0018). Written
2026-10-20 against `1e83f33`. Depends on proposal 03 phase 1 (a lib crate)
only so the contract tests have something to link; the design itself is
independent.

Issue #1 asks for interchangeable tools so an orchestrator can run paired
evals, starting with search-and-replace edits versus hashline edits. Its
analysis of the coupling is accurate. This document turns it into the smallest
Rust design that meets its completion criteria, and pushes back on two points.

## The coupling, by file and line

A tool's identity is a string that is retyped in seven places. Adding or
swapping one means touching all seven and remembering to.

| Concern | Where | Form |
|---|---|---|
| schema | `src/tools/mod.rs:120-185` (`core()`, `web_defs()`, `memory_def()`…) | `def("edit", …)` |
| dispatch | `src/tools/mod.rs:273-305` (`call`) | `match name { "edit" => fs::edit … }` |
| parallelism | `src/tools/mod.rs:269` (`is_readonly`) | `matches!(name, "read" \| "grep" …)` |
| group | `src/agent.rs:172-183` (`group_for_tool`) | `match name { "web_search" \| "web_fetch" => Group::Web …}` |
| TUI presentation | `src/tui/mod.rs:122-134, 656-672` (`primary_arg`, `tool_preview_lines`, `tool_body`) | `"edit" => out.lines().skip(1)…` (parses the edit tool's prose) |
| learning | `src/learn.rs:354, 477` | `call.function.name != "skill"` |
| scripted endpoint | `dev/src/serve.rs:292-300` (`adapt`) | harness-agnostic name aliases; fine as is |

Plus one shared implementation: `skills::Library::patch` calls
`tools::edit::replace_once` (src/skills.rs:187). The issue is right that an
editor experiment must not move skill patching; the fix is just to make that a
rule with a test, since the function is already not a tool.

## Design

Three types. Nothing else is new.

### `Tool` — one object per tool, owning everything about it

```rust
// crates/construct/src/tools/mod.rs

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Wire name the model calls. Declared here and nowhere else.
    fn name(&self) -> &'static str;
    fn group(&self) -> Group;
    /// The OpenAI function definition (today's `def(...)` value).
    fn definition(&self) -> Value;
    /// Changes nothing, so it may run beside other read-only calls.
    fn readonly(&self) -> bool { false }
    /// How the UI should show a call. Defaults are right for most tools.
    fn display(&self) -> Display { Display::default() }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError>;
}

/// Presentation hints, so the TUI never matches on names.
#[derive(Default)]
pub struct Display {
    /// Argument shown on the call line ("command" for shell, "path" for read…).
    pub primary_arg: &'static str,
    /// Lines of output to show under a finished call (0 = collapse).
    pub preview_lines: usize,
}

pub struct Output {
    /// What the model receives. Unchanged from today.
    pub text: String,
    /// How the UI renders it. The edit tool returns a Diff; shell a Tail.
    pub kind: OutputKind,            // Plain | Diff | Tail
    /// Machine-readable result for logs and scorers (proposal 05):
    /// e.g. {"status":"applied","ops":3} or {"status":"rejected","code":"ANCHOR_MISMATCH"}.
    pub data: Option<Value>,
}

pub struct ToolError {
    /// Prose for the model, as today ("old_string not found. It must match…").
    pub message: String,
    /// Stable code for scorers; free-form, tool-defined ("not_found", "ambiguous", "sandbox").
    pub code: &'static str,
}
```

`async_trait` is one well-known dependency and makes the trait object-safe
without every implementer writing `Pin<Box<dyn Future>>`. The per-call box is
noise next to a process spawn or an HTTP request.

Each tool is one file that owns its schema, its argument parsing, its
execution and its display hints, in that order, so a reader sees the whole
contract in one screen:

```
tools/
├── mod.rs          Tool, Display, Output, ToolError, Group, Catalog, cap_output
├── shell.rs        Shell
├── read.rs         Read            (plain, line-numbered)
├── write.rs        Write
├── edit/
│   ├── mod.rs      Edit            (old_string/new_string; wraps replace::replace_once)
│   ├── replace.rs  replace_once + tiers — a plain function, also used by skills.rs, never swapped
│   └── hashline/   HashlineRead, HashlineEdit, and their anchor/apply/format modules
├── grep.rs, glob.rs
├── web.rs          WebSearch, WebFetch
├── memory.rs       Memory
├── skill.rs        SkillView, SkillManage
├── history.rs      SessionSearch
└── mcp.rs          McpTool (one instance per discovered remote tool)
```

### `Catalog` — the tools this session has

```rust
pub struct Catalog { tools: Vec<Arc<dyn Tool>> }   // stable order = definition order in requests

impl Catalog {
    /// The one place implementations are chosen. Reads config, returns tools.
    pub fn build(cfg: &ToolsConfig, mcp: Option<&mcp::Manager>) -> Catalog;
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>>;
    pub fn definitions(&self, enabled: &BTreeSet<Group>) -> Vec<Value>;
    pub fn group_of(&self, name: &str) -> Option<Group>;      // replaces Agent::group_for_tool
    pub fn offerable(&self, enabled: &BTreeSet<Group>) -> Vec<(Group, String)>;  // for enable_tools
}
```

`build` is a plain `match` on config, not a registry:

```rust
match cfg.editor.as_str() {
    "replace"  => { tools.push(Arc::new(Read)); tools.push(Arc::new(Edit)); }
    "hashline" => { tools.push(Arc::new(HashlineRead::new(&cfg.hashline))); tools.push(Arc::new(HashlineEdit::new(&cfg.hashline))); }
    other => bail!("tools.editor: unknown editor {other:?}"),
}
tools.retain(|t| !cfg.disabled.iter().any(|d| d == t.name()));
```

Note that `editor` selects a *pair*. Hashline is a read format plus an edit
protocol; the issue says this and the design makes it structural: a config key
chooses a family, never a single tool, when the tools only make sense together.

The agent loop, the learning loop and the curator all hold a `Catalog` and
only ever call `get`, `definitions`, `group_of`. `tools::call(ctx, name, args)`
becomes `catalog.get(name)?.call(ctx, args)`; an unknown or disabled tool is
"unknown tool" in both loops, and `group_of` returning `None` means the model
cannot switch it back on. That is completion criteria 2 and 3 with no extra
policy layer: a tool that is not in the catalog does not exist.

MCP: `Manager::ensure(server)` already discovers remote tools lazily. On
discovery it returns `Vec<McpTool>` and the agent calls `catalog.extend(..)`;
`Catalog` holds a `RwLock<Vec<..>>` for that one case. Do this second; the
editor experiment does not need it, and the `mcp__` prefix branch in `call`
can stay for one more PR. (As built: `Catalog::start_mcp(server)` starts the
server and adds a `tools::mcp::McpTool` per listed tool; a name the server
did not list is an unknown tool.)

### `ToolsConfig` — the profile is the config

Issue #1 proposes "named experiment profiles" as a new entity. construct
already has the entity: `config.toml` in a `CONSTRUCT_HOME`, and
`bench.py` already writes one per harness home (`Harness.configure`). A
profile is a config file. Adding a second naming layer would mean two places
that can disagree.

```toml
[tools]
editor = "replace"            # or "hashline"
disabled = []                 # tool names that must not exist this session, even if the model asks
web = "native"                # future: "mcp:fetcher" to run the web pair through an MCP server

[tools.hashline]
anchor = "line+hash"          # or "hash" — the open question in issue #1's comment, as a knob
min_hash_len = 2
window_radius = 4
context_lines = 1
```

`CONSTRUCT_TOOLS_EDITOR=hashline` as an env override for one-off runs, in the
style of the existing `CONSTRUCT_ROUTER`.

The issue's four states map onto things that already exist:

| issue term | construct |
|---|---|
| installed | compiled into `Catalog::build`'s match arms |
| allowed | `[tools]` config: `editor`, `disabled` |
| exposed | router groups, per turn, as today |
| selected implementation | `editor`, `web` keys |

### Provenance

Every result needs to say what produced it. Add a `Manifest` emitted once:

```rust
pub struct Manifest { git_sha, git_dirty, binary_sha256, config_sha256, tools: Vec<String>, editor, model, router_mode }
```

- as the first line of every session log (session.rs header, next to `harness`/`model`),
- as an `Event::Manifest` right after `TurnStart` on the first turn (so `--json` consumers get it),
- from `construct manifest` for the bench to record (proposal 05).

`git_sha` comes from a `build.rs` that reads `git rev-parse HEAD` at compile
time into `env!("CONSTRUCT_GIT_SHA")`; today the observations log carries
shas typed by hand.

## What moves where (mechanical)

| today | after |
|---|---|
| `tools::definitions(groups, ctx)` | `catalog.definitions(groups)` |
| `tools::call(ctx, name, args)` | `catalog.get(name).ok_or(..)?.call(ctx, args)` |
| `tools::is_readonly(name)` | `tool.readonly()` |
| `Agent::group_for_tool`, `Agent::offerable` | `catalog.group_of`, `catalog.offerable` |
| `tui::primary_arg(name, args)` | `tool.display().primary_arg` looked up through the catalog the TUI already shares via `Agent` |
| `tui::tool_preview_lines`, `tui::tool_body` | `display().preview_lines`, `Output.kind` carried on `Event::ToolEnd` |
| `learn.rs:354` skip "skill"/"skill_manage list" | skip when `tool.readonly()` |
| `learn.rs:477` `skills_loaded` matches `"skill"` | `SkillView::NAME` constant |
| `Event::ToolEnd { ok, output, ms }` | add `kind: OutputKind, data: Option<Value>, code: Option<&str>` |

`Event::ToolEnd` gaining fields is the only wire change; `--json` consumers
and the session log get strictly more.

## Rules

1. **Wire strings are declared once.** `impl Tool { fn name() }` is the only
   place a tool's name appears as a literal. Tests may use it; other modules
   use `Shell::NAME`.
2. **No module matches on tool names.** Presentation uses `Display` and
   `OutputKind`; policy uses `Group` and `readonly()`. A `match name {` outside
   `tools/` fails review.
3. **A family swaps as a unit.** If two tools only work together (read and
   edit under hashline), one config key selects both.
4. **Skill patching does not go through a tool.** `skills.rs` calls
   `tools::edit::replace::replace_once` directly, and `tests/skill_patch.rs`
   pins that: patching a skill under `editor = "hashline"` behaves exactly as
   under `"replace"`.
5. **A tool's contract has a test that does not need a model.** Every
   `impl Tool` ships `tests/<tool>_contract.rs`: schema is valid JSON Schema,
   required args are enforced with the prose errors the model relies on,
   output is under `OUTPUT_MAX`, and the tool refuses paths the sandbox denies.
6. **Structured results, prose text.** `Output.text` is for the model and may
   change wording freely; `Output.data` and `ToolError.code` are for scorers
   and change only with a note in the ADR.

## The first experiment: replace vs hashline

### Porting hashline

Port `omp-hashline-strict` (the user's TypeScript implementation) module for
module: `hash.ts` → `hashline/hash.rs` (FNV-1a 64 to base-26), `anchors.ts` →
`anchors.rs`, `ops.ts` → `ops.rs`, `apply.ts` → `apply.rs`, `format.ts` →
`format.rs`, `fsio.ts` → use `config::write_atomic` and preserve EOL/BOM in
`apply`. Its 64 `node:test` cases become the contract test; port them first,
then make them pass. The tool descriptions in `prompts.ts` become the
`definition()` text.

Out of scope for v1: echoed-anchor stripping config (`stripEchoes`) stays on,
and `read` delegation to selectors/archives does not apply (construct's read
is plain text).

### On dropping line numbers

The issue comment wonders whether `LINE:HASH` anchors could become just
`HASH`, and remembers there was a reason not to. I believe the reason is this:
a hash-only anchor matches wherever that content is *now*, so an edit follows
content that has moved. That is relocation, and strict hashline exists to
remove relocation, because silent relocation is how an edit lands on the wrong
line without anyone noticing. The line pins *where*, the hash pins *what*, and
a mismatch in either is loud. Two smaller reasons: hash-only anchors must be
unique across the whole file rather than merely verify one line, so displayed
hashes get longer and every read line costs more tokens; and models have seen
far more `cat -n` output than anything else, so line numbers help them
navigate `offset`/`limit` reads.

None of that needs to be decided. `tools.hashline.anchor = "line+hash" |
"hash"` is a config knob, and the bench runs both arms. This is the whole
point of the harness.

### Running the comparison

Three arms, same tasks, same model, same seed, interleaved rounds:

| arm | config |
|---|---|
| `replace` | `editor = "replace"` |
| `hashline` | `editor = "hashline"`, `anchor = "line+hash"` |
| `hashline-hashonly` | `editor = "hashline"`, `anchor = "hash"` |

Learning is frozen (`[learn] enabled = false`) for the editor comparison so a
skill written in round 1 cannot help round 3.

Metrics come from two deterministic sources, never from reading transcripts:

- **outcome**: the task's scripted check (proposal 05), unintended file
  changes (`git status --porcelain` in the work dir minus the task's expected
  paths),
- **process**: from `Event::ToolEnd.data` in the session log: edit calls,
  rejections by `code` (`ANCHOR_MISMATCH`, `not_found`, `ambiguous`),
  recoveries (a rejection followed by an accepted edit of the same path within
  two steps), re-reads after a rejection, forgiveness events
  (`echo_stripped`), tokens per edit from the endpoint records.

The task corpus for this experiment needs the cases the issue lists: duplicate
lines, whitespace-only and Unicode changes, a block shifted by an insertion
above, a stale reference (the task's `check.sh` mutates the file between
turns), and several ordinary multi-edit changes. Six to ten small tasks, each
with a `check.sh`. Issue #15's realistic-project suite is the second corpus,
not a prerequisite.

## Order of work

1. **Trait + catalog, native tools only, no new behaviour.** Move each
   existing tool into its file, implement `Tool`, replace the seven match
   sites, add `Display`/`OutputKind`, extend `Event::ToolEnd`. e2e stays
   green, golden request snapshots (proposal 05) unchanged. This is the PR
   that proves the refactor is a refactor.
2. **`[tools]` config, `disabled`, `Manifest`.** e2e: a disabled tool is
   absent from definitions, "unknown tool" when called, and not offered by
   `enable_tools`; the learning pass sees the same catalog.
3. **`tests/skill_patch.rs`** and the contract tests for existing tools.
4. **Hashline port** behind `editor = "hashline"`, with its ported tests and
   one e2e scenario (scripted model issues hashline-shaped calls, a stale
   anchor is rejected with a window, the retry lands).
5. **MCP tools through the catalog.**
6. **Bench arms** (proposal 05) and the first paired run. Write the ADR with
   the numbers, whichever way they go.

Steps 1 to 3 are the decoupling the issue asks for; they are mechanical and
good first PRs for a model that is careful but not inventive. Step 4 is the
one that needs judgement.
