//! What a file tool is told when the policy says no: the path, where to work
//! instead, and how the user can open it up. The text is prose for the model,
//! so it names the scratch dir when the path was a temp one: a model reaching
//! for /tmp wants scratch space, and the sandbox has some.

use super::Policy;
use std::path::Path;

impl Policy {
    pub(super) fn denied(&self, p: &Path, what: &str) -> String {
        // macOS resolves /tmp to /private/tmp before the check, so compare both forms.
        let temp = [Path::new("/tmp").into(), Path::new("/var/tmp").into(), std::env::temp_dir()];
        let under = |t: &std::path::PathBuf| p.starts_with(t) || t.canonicalize().is_ok_and(|c| p.starts_with(c));
        let scratch = if temp.iter().any(under) {
            format!(" For scratch files use {}, which is writable ($TMPDIR in shell commands).", self.tmp.display())
        } else {
            String::new()
        };
        format!(
            "blocked by the sandbox: {} is outside the workspace ({what}). Work inside {}.{scratch} If the user wants this path available, they can add it to [sandbox] allow_{what} in construct's config.toml.",
            p.display(),
            self.read.first().map(|p| p.display().to_string()).unwrap_or_default()
        )
    }
}
