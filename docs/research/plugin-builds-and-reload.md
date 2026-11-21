# Smaller Construct builds and tools learned at runtime

Read November 20, 2026. [#100](https://github.com/strf0x1/construct/issues/100) asks how to try interchangeable implementations. [#272](https://github.com/strf0x1/construct/issues/272) adds a distribution question. [#186](https://github.com/strf0x1/construct/issues/186) proposes turning repeated tool calls into a project tool. Each can proceed independently.

## What ships today

Yes, Rust compiles the hashline implementation even when the user selects replace. Both editor families are Rust modules and both constructors are reachable from a runtime config match. The catalog builds tools, then applies the disabled list. Routing further controls which definitions the model sees. None of those decisions tells Cargo to omit a module or its dependencies. Release LTO can discard unreachable machine code, so a source line count cannot tell us the bytes saved by removing hashline. [Editor modules][editor] · [Catalog construction][catalog]

The checkout at `d7c52d5` predates the split. Upstream at `29ff867` already has the binary composition root, engine crate and TUI crate planned in #274. The binary still links the TUI. Building that revision with `cargo build -p construct --release --no-default-features` drops the keyring dependency and reads credentials from the environment. It still compiles hashline and optchat. ADR 0052 reports about 8.8 MB with keyring and 6.9 MB env-only on arm64 macOS. Those are existing measurements, not estimates for a five-tool Linux build. [#274](https://github.com/strf0x1/construct/issues/274) · [Binary manifest][bin] · [Core manifest][core] · [Size measurements][size]

## A five-tool distribution

I would keep `read`, replace-mode `edit`, `grep`, `glob` and `bash` as the base for a headless Linux build. The regular distribution can enable the other families by default. Start with additive Cargo features in the existing crates. Gate both module declarations and registration, and make an exclusive dependency optional. A web feature, for example, can own the HTML converter while the model client still needs HTTP and TLS. Forward features from the binary so a packager can choose a supported bundle. A new workspace crate is worthwhile when an extension can depend on the core's interfaces without the core depending back on it. [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html) · [Core dependencies][core] · [Composition plan](https://github.com/strf0x1/construct/issues/274)

Count the names on a real model request. Both editor branches currently register `write`, agent construction can add `delegate`, and request construction adds strategy tools plus `enable_tools` when groups remain available. For exactly five, use compact, exclude those registrations and disable discovery of extra groups. Check dispatch too. A model can request a tool it hasn't seen. Keep bash's workspace sandbox. Its background option uses the job engine even if the separate job tool is absent. A five-name interface and a smaller engine are distinct measurements. [Catalog][catalog] · [Agent construction][restore] · [Request definitions][request] · [Shell][shell]

A local check on the older installed build `36487badf52d` confirms the runtime distinction. With a fresh home, a scripted endpoint and the other tools disabled, the request offered exactly `shell` (since renamed to `bash`), `read`, `edit`, `grep` and `glob`. Changing the editor config on that same executable changed the edit arguments from `old_string` and `new_string` to an `edits` array.

Compare a regular release, env-only release, and five-tool prototype on the same Linux target. Record stripped bytes, dependency features, startup and peak RSS for the same scripted edit. Snapshot the actual five definitions and verify a call to an omitted tool fails. A separate config error should name a requested editor or strategy missing from that build. These measurements would establish whether another crate split or feature gate is worth maintaining.

## Where optchat belongs

Keep optchat as an optional compiled `Context` implementation. Builds that include both can still select either at session creation. On the recorded 140-turn coordinator task optchat recovered 24/24 facts versus compact's 8/24 across three runs per arm, at median costs of $0.344 and $0.194 respectively. That is a reason to keep the experiment available. It does not settle the default for ordinary editing. A session records its strategy in its journal. A build lacking optchat should explain which build is needed to resume it. Switching the live implementation would require a design for migrating its tree and replaying its recorded events. [Context construction and replay][context] · [ADR 0042][strategy] · [ADR 0043 results][optchat]

## What to reload

Bohdan Triapitsyn describes OpenChamber's migration to OpenCode 2. An active session sees a saved change on its next step. When one MCP server changes, OpenCode reconnects that server. The UI reports which plugins loaded and why a load failed. Those semantics translate to Construct independent of the implementation language. OpenChamber also describes Code Mode, where a short program composes tools during one call. Saving and routing a program for later sessions is a separate feature. [OpenChamber release article](https://openchamber.dev/blog/opencode-v2/)

| Rust extension mechanism | Where it fits in Construct |
|---|---|
| Statically linked module or optional crate | Built-in editors and context strategies can use the existing traits. Shipping a new implementation requires rebuilding the binary. [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html) |
| Sandboxed stdio process | External tools can already implement MCP in another language. Construct starts a configured server lazily and lists its tools. Today it ignores list-change notifications. Refreshing a running server's catalog would be new work. [MCP client][mcp] · [MCP tools specification](https://modelcontextprotocol.io/specification/2025-06-18/server/tools) |
| Wasm component | A typed guest can import a small set of host functions. The host must supply the runtime, resource limits and permission checks. Measure that cost if a use case needs portable in-process code. [WIT interfaces](https://component-model.bytecodealliance.org/design/wit.html) |
| Native shared library | Independently built Rust libraries need an explicit compatible ABI, commonly a versioned C interface. Exporting today's Rust trait object gives no ABI stability guarantee. Native code also shares the host process's privileges. [Rust ABI reference](https://doc.rust-lang.org/reference/items/external-blocks.html#abi) |

## A tool the learner could write

Suppose a project repeatedly asks which tests exercise a changed config key. A journal pattern of grep, glob and several reads gives the learner evidence to propose a project tool taking a key and returning matching test paths. Today a skill can contain a script, but saving it does not register a function schema. #186 proposes a named, routed script. #267 explores a runtime for calling existing tools from a short program. A structured recipe over those three read-only tools would also test whether learned tools earn their place before adding an interpreter. [Skill storage][skills] · [Learned tools](https://github.com/strf0x1/construct/issues/186) · [Code Mode experiment](https://github.com/strf0x1/construct/issues/267)

For a first experiment, persist the candidate's schema, implementation digest and a fixture with its expected answer. On a later session, expose the passing version only when routed. Invoke its children under the caller's normal tool policy and journal which version and calls produced the result. Compare held-out correctness, rounds, cost and reuse against the same project skill invoked conventionally. Then try refreshing between model requests inside one session. Bind each request's advertised schemas to the same implementations through dispatch, and keep a working version if a replacement fails validation. This lets the user try a tool on the next turn without changing the meaning of a call the model has already issued. [Tool group behavior][groups] · [Learning experiment](https://github.com/strf0x1/construct/issues/186)

[editor]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/tools/edit/mod.rs
[catalog]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/tools/catalog.rs
[bin]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct/Cargo.toml
[core]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/Cargo.toml
[size]: https://github.com/strf0x1/construct/blob/29ff867/docs/adr/0052-release-opt-z.md
[restore]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/agent/restore.rs
[request]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/agent.rs
[shell]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/tools/shell.rs
[context]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/context/mod.rs
[strategy]: https://github.com/strf0x1/construct/blob/29ff867/docs/adr/0042-context-strategy.md
[optchat]: https://github.com/strf0x1/construct/blob/29ff867/docs/adr/0043-optchat-context-strategy.md
[mcp]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/mcp/mod.rs
[skills]: https://github.com/strf0x1/construct/blob/29ff867/crates/construct-core/src/skills.rs
[groups]: https://github.com/strf0x1/construct/blob/29ff867/docs/adr/0005-tool-groups-only-grow.md
