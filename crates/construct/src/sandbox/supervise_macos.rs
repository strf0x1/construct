//! The macOS per-call supervisor: the shell runs in its own process group, and
//! the group is killed when the call ends.
//!
//! macOS has no subreaper and no parent-death signal, so this is weaker than
//! the Linux supervisor (`supervise.rs`): a process that leaves the group
//! (`setsid`, `daemon`) is not swept (#9). What it does hold:
//! - the shell's group is SIGKILLed when the shell exits, when construct asks
//!   the helper to stop (SIGTERM), and when construct dies;
//! - all of that is one `kevent` wait, with no polling: the shell's exit,
//!   construct's exit and the stop signals are kqueue events.
//!
//! What was killed goes to the helper's stderr as `SWEPT <pid> <comm>` lines,
//! as on Linux.

use std::ffi::c_int;
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;

/// Prefix of a status line naming a process the sweep stopped.
pub const SWEPT: &str = "construct-swept";

/// libproc's `PROC_PGRP_ONLY` (the libc crate does not export it).
const PROC_PGRP_ONLY: u32 = 2;
const STOP_SIGNALS: [c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// A handler that does nothing. EVFILT_SIGNAL sees a signal whose action is a
/// handler, and a handler (unlike SIG_IGN) is reset to the default on exec, so
/// the shell gets default dispositions.
extern "C" fn noop(_: c_int) {}

fn event(ident: usize, filter: i16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// Register `changes`; false if the kernel refused (a process already gone).
fn register(kq: c_int, changes: &[libc::kevent]) -> bool {
    // SAFETY: `changes` is a valid array; no events are requested.
    unsafe {
        libc::kevent(kq, changes.as_ptr(), changes.len() as c_int, std::ptr::null_mut(), 0, std::ptr::null()) >= 0
    }
}

/// Run `prog` as a supervised child and exit with its status. `parent` is
/// construct's pid (0 to skip the check). `merge_stderr` sends the program's
/// stderr to stdout (shell calls); otherwise it keeps the helper's.
pub fn run(parent: u32, prog: &[String], merge_stderr: bool) -> ! {
    // SAFETY: plain syscalls; this process is single-threaded (see main.rs).
    let kq = unsafe {
        for s in STOP_SIGNALS {
            libc::signal(s, noop as extern "C" fn(c_int) as usize);
        }
        libc::kqueue()
    };
    let mut changes: Vec<libc::kevent> =
        STOP_SIGNALS.iter().map(|&s| event(s as usize, libc::EVFILT_SIGNAL, 0)).collect();
    // SAFETY: getppid cannot fail.
    let orphaned = || parent != 0 && unsafe { libc::getppid() } as u32 != parent;
    if parent != 0 {
        changes.push(event(parent as usize, libc::EVFILT_PROC, libc::NOTE_EXIT));
    }
    // A refused registration is construct already gone (or kqueue unavailable,
    // which would leave nothing to wait on).
    if kq < 0 || !register(kq, &changes) || orphaned() {
        std::process::exit(129);
    }
    let stderr = std::io::stdout().as_fd().try_clone_to_owned();
    let mut cmd = std::process::Command::new(&prog[0]);
    cmd.args(&prog[1..]).process_group(0);
    if let Ok(fd) = stderr
        && merge_stderr
    {
        cmd.stderr(fd);
    }
    let shell = match cmd.spawn() {
        Ok(c) => c.id() as i32,
        Err(e) => {
            eprintln!("construct sandbox: exec {}: {e}", prog[0]);
            std::process::exit(126)
        }
    };
    // Refused when the shell has already exited; the wait below sees that.
    register(kq, &[event(shell as usize, libc::EVFILT_PROC, libc::NOTE_EXIT)]);
    let mut status = None;
    while status.is_none() {
        if try_wait(shell, &mut status) || orphaned() {
            break;
        }
        let mut ev: libc::kevent = event(0, 0, 0);
        // SAFETY: waits for one event into `ev`; no timeout.
        let n = unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut ev, 1, std::ptr::null()) };
        if n > 0 && ev.filter == libc::EVFILT_SIGNAL {
            break;
        }
        if n > 0 && ev.filter == libc::EVFILT_PROC && ev.ident == parent as usize {
            break;
        }
    }
    for (pid, comm) in sweep(shell) {
        eprintln!("{SWEPT} {pid} {comm}");
    }
    // The shell may still be running (we were told to stop); it is in the
    // killed group, so this returns.
    if status.is_none() {
        let mut st = 0;
        // SAFETY: waits for our own child, killed above.
        if unsafe { libc::waitpid(shell, &mut st, 0) } == shell {
            status = Some(st);
        }
    }
    exit_like(status)
}

fn try_wait(shell: i32, status: &mut Option<i32>) -> bool {
    let mut st = 0;
    // SAFETY: plain non-blocking waitpid on our child.
    if unsafe { libc::waitpid(shell, &mut st, libc::WNOHANG) } == shell {
        *status = Some(st);
        return true;
    }
    false
}

/// SIGKILL the shell's process group and name what was in it besides the
/// shell. The group id is the shell's pid and cannot be reused while the
/// shell is unreaped or any member remains.
fn sweep(shell: i32) -> Vec<(i32, String)> {
    let named: Vec<(i32, String)> = group(shell).into_iter().filter(|&p| p != shell).map(|p| (p, comm(p))).collect();
    // SAFETY: signals the group we created for the shell.
    unsafe { libc::killpg(shell, libc::SIGKILL) };
    named
}

/// Pids in process group `pgid`, from libproc.
fn group(pgid: i32) -> Vec<i32> {
    // SAFETY: a size query, then a read into a buffer of that size.
    unsafe {
        let want = libc::proc_listpids(PROC_PGRP_ONLY, pgid as u32, std::ptr::null_mut(), 0);
        if want <= 0 {
            return Vec::new();
        }
        let mut pids = vec![0i32; want as usize / size_of::<i32>() + 8];
        let got = libc::proc_listpids(
            PROC_PGRP_ONLY,
            pgid as u32,
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<i32>()) as c_int,
        );
        pids.truncate(got.max(0) as usize / size_of::<i32>());
        pids.retain(|&p| p > 0);
        pids
    }
}

fn comm(pid: i32) -> String {
    let mut buf = [0u8; 256];
    // SAFETY: libproc writes at most buf.len() bytes.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned()
}

/// Exit the way the shell did: its code, or the same signal re-raised.
fn exit_like(status: Option<i32>) -> ! {
    let Some(st) = status else { std::process::exit(125) };
    if libc::WIFSIGNALED(st) {
        let sig = libc::WTERMSIG(st);
        // SAFETY: restore the default action and deliver the signal to ourselves.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
        std::process::exit(128 + sig)
    }
    std::process::exit(libc::WEXITSTATUS(st))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_in_its_own_group_listing() {
        // SAFETY: getpgrp cannot fail.
        let pgid = unsafe { libc::getpgrp() };
        let me = std::process::id() as i32;
        assert!(group(pgid).contains(&me));
        assert!(!comm(me).is_empty());
    }
}
