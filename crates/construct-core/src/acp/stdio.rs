//! `construct acp`: the ACP server (`server::serve`) on stdin and stdout.
//! Stdin closing closes the server's input, which shuts it down; a peer's
//! link uses `serve` directly and detaches instead (ADR 0071).
//!
//! The design choice: the protocol owns stdout. The writer thread keeps a
//! private duplicate of fd 1 and fd 1 itself is pointed at stderr, so a
//! `println!` or a dependency's log line becomes a diagnostic instead of a
//! frame a strict client cannot parse (#162, ADR 0035).

use super::server::{Input, serve};
use crate::config::{Config, Paths};
use crate::tools::ToolSources;
use anyhow::Result;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// `construct acp` on stdin and stdout. Stdin closing shuts down.
pub async fn run(cfg: Config, paths: Paths, sources: ToolSources) -> Result<()> {
    let (out, mut lines_out) = mpsc::unbounded_channel::<String>();
    // Only this loop writes protocol frames; diagnostic writes go to stderr
    // because `claim_stdout` points fd 1 there.
    let protocol = claim_stdout();
    let writer = std::thread::spawn(move || {
        let mut w: Box<dyn Write> = match protocol {
            Some(fd) => Box::new(std::fs::File::from(fd)),
            None => Box::new(std::io::stdout()),
        };
        while let Some(l) = lines_out.blocking_recv() {
            if w.write_all(l.as_bytes()).and_then(|_| w.flush()).is_err() {
                break;
            }
        }
    });
    let (input, lines_in) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if input.send(Input::Line(line)).is_err() {
                break;
            }
        }
    });
    let done = serve(cfg, paths, sources, lines_in, out).await;
    reader.abort();
    let _ = writer.join();
    done
}

/// Take the protocol's own handle on stdout, then point fd 1 at stderr, so a
/// `println!` or a dependency's log becomes a diagnostic instead of a frame
/// (ADR 0035). `None` when the descriptor could not be taken or repointed; fd
/// 1 is unchanged then and the caller falls back to it.
fn claim_stdout() -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    let protocol = std::io::stdout().as_fd().try_clone_to_owned().ok()?;
    // SAFETY: both descriptors are open for the life of the process.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } == -1 {
        // fd 1 still names the protocol stream. Say so and return `None`: the
        // caller keeps writing frames there, exactly as before the change, so
        // the failure is a missing guarantee rather than a broken session.
        eprintln!(
            "construct acp: could not point stdout at stderr ({}); diagnostics can reach the protocol stream",
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some(protocol)
}
