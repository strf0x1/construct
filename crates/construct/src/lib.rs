//! construct: a lean, self-improving agent harness. An OpenAI-compatible
//! streaming loop with in-process tools, capped memory and agent-written
//! skills, a background learning pass, a Jev/local router, and a full-screen
//! TUI. The binary is a thin `main.rs` over this library; see [`cli::run`].

pub mod agent;
pub mod cli;
pub mod compact;
pub mod config;
pub mod event;
pub mod jev;
pub mod journal;
pub mod learn;
pub mod llm;
pub mod manifest;
pub mod mcp;
pub mod memory;
pub mod project;
pub mod prompt;
pub mod router;
pub mod sandbox;
pub mod secrets;
pub mod skills;
pub mod tools;
pub mod tui;
