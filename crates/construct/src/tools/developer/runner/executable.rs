//! Resolve host executables outside writable roots and bind approval to their bytes.

use crate::tools::{Ctx, ToolError};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub struct Executable {
    pub path: PathBuf,
    digest: Vec<u8>,
}

impl Executable {
    #[cfg(test)]
    pub fn fake(path: PathBuf) -> Self {
        let bytes = std::fs::read(&path).unwrap();
        Self { path, digest: Sha256::digest(bytes).to_vec() }
    }

    pub fn find(name: &str, ctx: &Ctx) -> Result<Self, ToolError> {
        let paths = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&paths) {
            if !dir.is_absolute() {
                continue;
            }
            let Ok(path) = dir.join(name).canonicalize() else { continue };
            // A workspace cannot install or replace the authenticated runner.
            if path.starts_with(&ctx.cwd)
                || ctx.policy().write.iter().any(|root| path.starts_with(root))
                || !path.is_file()
            {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|_| "cannot read developer executable")?;
            return Ok(Self { path, digest: Sha256::digest(bytes).to_vec() });
        }
        Err(ToolError::new("not_found", format!("host {name} executable not found outside the workspace")))
    }

    pub fn check(&self) -> Result<(), ToolError> {
        let bytes = std::fs::read(&self.path).map_err(|_| "approved executable is no longer readable")?;
        if Sha256::digest(bytes).as_slice() != self.digest {
            return Err(ToolError::new(
                "changed",
                "approved executable changed; prepare and approve the operation again",
            ));
        }
        Ok(())
    }

    pub fn identity(&self) -> String {
        format!(
            "{} (sha256 {})",
            self.path.display(),
            self.digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    }
}
