//! construct-core: the engine of a lean, self-improving agent harness. An
//! OpenAI-compatible streaming loop with in-process tools, capped memory and
//! agent-written skills, a background learning pass, a Jev/local router. The
//! `construct` binary is a thin composition root over this library
//! (`crates/construct`); the TUI front end lives in `crates/construct-tui`.

pub mod acp;
pub mod agent;
pub mod agents;
pub mod child;
pub mod clock;
pub mod compact;
pub mod config;
pub mod context;
pub mod endpoint;
pub mod event;
pub mod guard;
pub mod home;
pub mod http;
pub mod jev;
pub mod jobs;
pub mod journal;
pub mod learn;
pub mod llm;
pub mod manifest;
pub mod mcp;
pub mod memory;
pub mod models;
pub mod permission;
pub mod project;
pub mod prompt;
pub mod providers;
pub mod router;
pub mod sandbox;
pub mod secrets;
pub mod skills;
pub mod text;
pub mod tools;
pub mod trace;
