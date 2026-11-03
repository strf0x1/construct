//! The vendored ACP extension contract: a pin in `CONTRACT.lock` and a copied
//! matrix/schema tree. The gate runs `scripts/acp-contract.sh verify`, which
//! hashes the tree, so a hand edit or a stale copy fails instead of passing.

use std::path::Path;
use std::process::Command;

/// Empty when the vendored tree matches its pin; otherwise the reason.
pub fn problems(root: &Path) -> Vec<String> {
    let script = root.join("scripts/acp-contract.sh");
    match Command::new("bash").arg(&script).arg("verify").current_dir(root).output() {
        Ok(o) if o.status.success() => Vec::new(),
        Ok(o) => vec![format!("{}: {}", script.display(), String::from_utf8_lossy(&o.stderr).trim())],
        Err(e) => vec![format!("{}: {e}", script.display())],
    }
}
