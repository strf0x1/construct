# Code mode in Droid and opencode, against ours (#267)

Read 2026-11-19. Droid is closed source: what follows is from Factory's own
post ([Droid 1.0: Building a Frontier Harness](https://factory.com/news/frontier-harness),
2026-10-29) and [release notes](https://docs.factory.com/docs/changelog/release-notes).
opencode is from source: `anomalyco/opencode` at `6baeebf`
(`packages/codemode/`, `packages/core/src/codemode/`, `packages/core/src/tool.ts`).

## What each one does

| | construct (838e18c) | Droid 1.0 "Script" | opencode 2.x `execute` |
|---|---|---|---|
| Default | off | on, but only for a list of frontier models (GPT-6.x, Claude Fable/Opus 5.x, Kimi K3); other models are chosen per model | on |
| Which tools go inside | all of them (`only`) or none hidden (`optional`) | Script and direct tools are both offered; the model is tuned to choose Script. 2 tool definitions up front, the rest load on demand | MCP, OpenAPI and opencode's own API tools. Core tools (read, edit, write, patch, shell, grep, glob, todo, subagent, webfetch, skill) stay direct (`codemode: false`); they join only under the experimental `full_codemode` flag |
| Engine | QuickJS, fresh per call | JS sandbox, engine not named | its own interpreter (psychoscript): only the features it implements, no real JS engine |
| How the code is passed | JSON string field `code` | "raw input blocks, not escaped strings" for file contents, patches, commands | `freeform: { type: "text" }`: the code is the raw tool input, "no JSON or other wrapper" |
| How tools are declared | TS-style signatures in the tool description, generated from the input schema; no return types | not published | TS signatures in the system instructions, namespaced, with typed outputs; `search()` for a partial catalog; only deltas sent when the catalog changes |
| What a tool returns to the program | `{ok, output (text), data}` | edits return "the same short summary as a direct edit" | a schema-validated machine value (`output`), separate from the text the model would see; `write`/`edit` return a short confirmation |
| What returns to the model | the return value, then `console:` lines | only what the program prints (`text()`), capped; full results are logged where the model can search them; trimming keeps start and end | the value, warnings, then `Logs:`; `maxOutputBytes` with an in-band marker |
| Parallelism guidance | "may run together (Promise.all)" | independent calls run concurrently | "Run independent calls concurrently with `Promise.allSettled`" |
| Long work | 300 s, then stopped | waits up to 5 min then hands back output so far; can run in the background, with a tool to check, wait or stop it | suspend/resume with idempotent and durable tools |
| Permissions inside | each call through the normal prompt | per-call approval at run time, with real values shown for fixed inputs | `before` hooks; a denied call throws |

## What they measured

- Droid, Script vs direct: 15% lower cost on Claude Opus 5.5. Most of the saving
  comes from the 12.5% of scripts with six or more calls. The big wins are
  sweeps: 312 Linear calls in 2 requests instead of an estimated 36, and a
  5,384-job CI sweep with under 0.1% of results reaching the model. No pass-rate
  ablation was published; their Terminal-Bench 4 numbers compare whole harnesses.
- opencode: none published for code mode.
- Ours: on DeepSeek V4.1 Flash, code-only 14/17 against 16/17 off, 6% fewer
  requests; only 16 of 247 programs made 5+ calls.

## What this says about our three questions

1. **Tasks.** Both treat code mode as a tool for wide work. opencode doesn't even
   put the core coding tools in it by default, and Droid's saving is concentrated
   in the rare long scripts. Our tasks are edit-then-test loops, so the modest
   result matches Factory's own breakdown. We need a fan-out task family
   (sweeps, audits, many-file renames, MCP-heavy work) to see the effect.
2. **Model.** Droid turns Script on by default only for frontier models. DeepSeek
   Flash is the kind of model it leaves to a per-model choice.
3. **Implementation.** What they do that we don't:
   - raw-text code input instead of a JSON-escaped string (our template-literal
     failures, and the escaping tax on every program);
   - typed return values in the declarations, and structured results with short
     summaries for edits and writes;
   - an output cap with the full result kept and searchable (we have
     `recall_result` for this already);
   - `Promise.allSettled` as the stated way to batch;
   - prompts and in-script tool descriptions tuned per model;
   - long programs that run in the background instead of being stopped.

## Experiments worth running next

- A fan-out task family, then code-only vs off on it.
- The same three arms on one frontier model.
- Cheap implementation changes, A/B'd on the existing set: raw code input (if the
  dialect allows it), return types in the declarations, `allSettled` guidance,
  and capped output with a `recall_result` handle.
