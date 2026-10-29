//! Startup milestones for measuring time to first frame: `CONSTRUCT_TRACE=1`
//! appends each one to `$CONSTRUCT_HOME/trace.log`, in milliseconds since the
//! first mark. It lives outside `tui` so `main` can mark its own start without
//! depending on the front end; with the variable unset a mark costs one env read.

use crate::config::Paths;
use std::io::Write;
use std::time::Instant;

/// Record `what` if `CONSTRUCT_TRACE` is set; never fails.
pub fn mark(what: &str) {
    if std::env::var_os("CONSTRUCT_TRACE").is_none() {
        return;
    }
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed();
    if let Ok(p) = Paths::resolve()
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p.home.join("trace.log"))
    {
        let _ = writeln!(f, "{:>8.2}ms {what}", t.as_secs_f64() * 1000.0);
    }
}
