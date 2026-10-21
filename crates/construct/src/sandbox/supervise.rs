//! The per-call process supervisor: nothing a shell command starts outlives
//! the call.
//!
//! The sandbox helper does not exec the shell. It marks itself a child
//! subreaper, forks the shell, and waits. When the shell exits, when construct
//! asks it to stop (SIGTERM), or when construct dies, it kills every process
//! left in its tree: orphans, `setsid` included, reparent to the helper instead
//! of init, so they cannot leave. The design choice: the subreaper lives in
//! this short-lived helper, never in construct, so the learner and MCP servers
//! are never caught and tokio's own child reaping is never contended.
//!
//! Signals are blocked and taken with `sigwaitinfo`, with no handlers. What
//! was killed goes to the helper's stderr as `SWEPT <pid> <comm>` lines; the
//! shell gets stdout as its stderr, so it never writes there.

use std::collections::HashSet;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

/// Prefix of a status line naming a process the sweep stopped.
pub const SWEPT: &str = "construct-swept";
/// A sweep that hasn't converged by then gives up (a process stuck in
/// uninterruptible sleep, or a fork bomb outrunning it).
const SWEEP_CAP: Duration = Duration::from_secs(2);
/// The parent-death signal. Not SIGTERM: Linux sends it when the *thread*
/// that spawned the helper exits, and tokio retires threads while construct
/// lives on, so on receipt the helper checks that its parent really is gone.
const PDEATH: i32 = libc::SIGUSR1;

/// Run `prog` as a supervised child and exit with its status. `parent` is
/// construct's pid (0 to skip the check).
pub fn run(parent: u32, prog: &[String]) -> ! {
    let watched = sigset(&[libc::SIGCHLD, libc::SIGTERM, libc::SIGINT, libc::SIGHUP, PDEATH]);
    // SAFETY: plain syscalls; this process is single-threaded (see main.rs).
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        libc::prctl(libc::PR_SET_PDEATHSIG, PDEATH, 0, 0, 0);
        libc::sigprocmask(libc::SIG_BLOCK, &watched, std::ptr::null_mut());
    }
    // SAFETY: getppid cannot fail.
    let orphaned = || parent != 0 && unsafe { libc::getppid() } as u32 != parent;
    // construct may have died before the death signal was armed.
    if orphaned() {
        std::process::exit(129);
    }
    // std resets the signal mask in the child, so the shell sees none of this.
    let stderr = std::io::stdout().as_fd().try_clone_to_owned();
    let mut cmd = std::process::Command::new(&prog[0]);
    cmd.args(&prog[1..]);
    if let Ok(fd) = stderr {
        cmd.stderr(fd);
    }
    let shell = match cmd.spawn() {
        Ok(c) => c.id() as i32,
        Err(e) => {
            eprintln!("construct sandbox: exec {}: {e}", prog[0]);
            std::process::exit(126)
        }
    };
    let mut status = None;
    while status.is_none() {
        // SAFETY: waits for one of the signals blocked above.
        let sig = unsafe { libc::sigwaitinfo(&watched, std::ptr::null_mut()) };
        match sig {
            libc::SIGCHLD => reap(shell, &mut status),
            PDEATH if !orphaned() => {}
            -1 => {}
            _ => break,
        }
    }
    for (pid, comm) in sweep(shell, &mut status) {
        eprintln!("{SWEPT} {pid} {comm}");
    }
    exit_like(status)
}

/// Kill everything left in the tree until nothing is, or the cap passes.
/// Killing a process reparents its children to us, so repeating converges.
/// Kills come from a snapshot of our own unreaped children, whose pids cannot
/// be reused until we reap them; no SIGSTOP, which a dying tree can race.
fn sweep(shell: i32, status: &mut Option<i32>) -> Vec<(i32, String)> {
    let deadline = Instant::now() + SWEEP_CAP;
    let mut seen = HashSet::new();
    let mut stopped = Vec::new();
    loop {
        reap(shell, status);
        let kids = children();
        if kids.is_empty() || Instant::now() > deadline {
            break;
        }
        for pid in kids {
            if seen.insert(pid)
                && pid != shell
                && let Some((comm, state)) = stat(pid)
                && state != 'Z'
            {
                stopped.push((pid, comm));
            }
            // SAFETY: pid is our own unreaped child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    stopped
}

/// Reap every child that has exited, keeping the shell's wait status.
fn reap(shell: i32, status: &mut Option<i32>) {
    let mut st = 0;
    // SAFETY: plain non-blocking waitpid.
    while let pid @ 1.. = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) } {
        if pid == shell {
            *status = Some(st);
        }
    }
}

/// Our direct children, from /proc/self/task/<tid>/children when the kernel
/// has it, else by scanning every process's parent.
fn children() -> Vec<i32> {
    let me = std::process::id();
    let parse = |s: &str| s.split_whitespace().filter_map(|p| p.parse().ok()).collect();
    if let Ok(s) = std::fs::read_to_string(format!("/proc/self/task/{me}/children")) {
        return parse(&s);
    }
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| ppid(pid) == Some(me as i32))
        .collect()
}

/// `comm` and state letter from /proc/<pid>/stat. comm may hold spaces and
/// parentheses, so split at the last ')'.
fn stat(pid: i32) -> Option<(String, char)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (head, tail) = s.rsplit_once(')')?;
    let comm = head.split_once('(')?.1.to_string();
    Some((comm, tail.trim_start().chars().next()?))
}

fn ppid(pid: i32) -> Option<i32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    s.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

/// Exit the way the shell did: its code, or the same signal re-raised.
fn exit_like(status: Option<i32>) -> ! {
    let Some(st) = status else { std::process::exit(125) };
    if libc::WIFSIGNALED(st) {
        let sig = libc::WTERMSIG(st);
        // SAFETY: restore the default action and deliver the signal to ourselves.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            let one = sigset(&[sig]);
            libc::sigprocmask(libc::SIG_UNBLOCK, &one, std::ptr::null_mut());
            libc::raise(sig);
        }
        std::process::exit(128 + sig)
    }
    std::process::exit(libc::WEXITSTATUS(st))
}

fn sigset(sigs: &[i32]) -> libc::sigset_t {
    // SAFETY: sigemptyset initialises the set before use.
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for &s in sigs {
            libc::sigaddset(&mut set, s);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parses_this_process() {
        let me = std::process::id() as i32;
        let (comm, state) = stat(me).unwrap();
        assert!(!comm.is_empty());
        assert!(matches!(state, 'R' | 'S'));
        assert_eq!(ppid(me), Some(unsafe { libc::getppid() }));
    }
}
