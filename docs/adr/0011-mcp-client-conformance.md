# 0011 — What the stdio MCP client guarantees, and what it leaves out

Status: accepted, 2026-10-22. Implements #46 items 1–7 and its minor list.
The client stays hand-rolled (`crates/construct/src/mcp/`); #42 asks the
same question for the OpenAI client.

## Context

An audit against the 2025-06-18 spec (#46) found that the client worked for
well-behaved servers and failed badly otherwise. A crashed server left calls
waiting 300 s. A `ping` from a server went unanswered. One tool name with a
`.` in it, or one over 64 characters, could make the provider reject the
whole request with a 400. And one slow `npx` start blocked every other
server.

## Decision

- **A dead server fails fast and comes back.** One reader task owns stdout.
  When stdout ends, it marks the connection dead and fails every pending
  request, under the same lock new requests take, so none can slip in after
  the drain. The next call starts the server again. Lines are read as bytes,
  so a line that isn't valid UTF-8 or JSON is skipped and the connection
  stays up.
- **Server requests are answered.** `ping` gets `{}`. Any other request gets
  `-32601`, since the client declares no capabilities.
- **Wire names are API-safe.** `mcp::wire_name` replaces any character
  outside `[A-Za-z0-9_-]` with `_`. A name over 64 characters is cut to 55
  plus `_` and 8 hex digits of an FNV-1a hash of the original tool name.
  `McpTool` keeps the original name for dispatch. Server names are checked
  when the config loads: 1–32 characters of letters, digits, `-` and single
  `_`, so `server_of` always splits a wire name correctly.
- **Each server has its own slot.** A server's start holds only its own
  lock.
- **Calls end cleanly.** `tools/call` carries a `progressToken`, and the
  wait (`[mcp.<name>] timeout`, default 300 s) starts over on each progress
  notification. A call that times out or is dropped removes its pending
  entry and sends `notifications/cancelled`. `initialize` is never
  cancelled.
- **Initialize is checked.** The negotiated version must be 2025-06-18,
  2025-03-26 or 2024-11-05; any other version is an error, and the server is
  shut down. A server that doesn't declare `capabilities.tools` is never
  sent `tools/list`. Pagination stops after 100 pages or a repeated cursor.
- **Results keep their text.** Text blocks and embedded resources' text pass
  through. Resource links, binary resources, images and audio each become a
  one-line note. `structuredContent` is used when the content is empty.
- **Smaller fixes.** Shutdown closes stdin, waits 2 s, then SIGTERMs the
  sandbox helper. A startup error ends with the last 4 KB of the server's
  stderr. `$schema` is stripped from input schemas. `readOnlyHint` feeds
  `readonly()`, which only decides whether calls may run in parallel; it is
  a hint from the server, so it never relaxes policy.

## Not done, on purpose

- **`instructions` from initialize are dropped.** Putting them in front of
  the model is a prompt change (Tier 2), and it should come with a measured
  run.
- **`notifications/tools/list_changed` is ignored.** Tools are listed once
  per start, and groups only grow (ADR 0005).
- **`$ref` / `$defs` in schemas pass through unchanged.** Inlining them waits
  until a provider is seen rejecting one.
- **Resources, prompts, sampling and the HTTP transports** are not
  implemented. HTTP is the next addition; the id multiplexing carries over.

## Evidence

- e2e (`crates/construct-dev/tests/e2e.rs`, "mcp spec" section, against
  `construct-dev mcp-fake --spec`, whose default listing is unchanged so
  `routed_mcp` stays put):
  - The server's `ping` is answered and its `sampling/createMessage` refused.
  - The connection survives a non-UTF-8 line.
  - Rich content and `structuredContent` render.
  - `dotted.tool` and a 66-character tool name dispatch, every listed name
    is a valid function name, and no `$schema` is sent.
  - A crash fails the call with "MCP server closed" and the next call works.
  - Progress keeps a 2.5 s call alive under a 1 s timeout, while a silent
    call times out and the server logs `notifications/cancelled`.
  - A failed start reports the server's stderr, and an unsupported version is
    refused.
  - A server name with `__` is refused when the config loads.
- Unit: `wire_name` and `check_server_name`, plus `content::render`.
- `context7_answers_from_inside_the_sandbox` (`--ignored`, real network)
  still passes.
