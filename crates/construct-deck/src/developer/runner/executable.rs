//! Resolve host executables outside writable roots and bind approval to their bytes.

use construct_core::tools::{Ctx, ToolError};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub struct Executable {
    pub path: PathBuf,
    digest: Vec<u8>,
}

impl Executable {
    #[cfg(target_os = "macos")]
    pub fn selected(path: PathBuf, ctx: &Ctx) -> Result<Self, ToolError> {
        let path = path.canonicalize().map_err(|_| "cannot resolve the selected Git executable")?;
        Self::outside_workspace(&path, ctx).ok_or("the selected Git executable must be outside writable roots")?;
        let bytes = std::fs::read(&path).map_err(|_| "cannot read the selected Git executable")?;
        Ok(Self { path, digest: Sha256::digest(bytes).to_vec() })
    }

    fn outside_workspace(path: &std::path::Path, ctx: &Ctx) -> Option<()> {
        (!path.starts_with(ctx.cwd())
            && !ctx.policy().write.iter().any(|root| path.starts_with(root))
            && path.is_file())
        .then_some(())
    }

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
            if Self::outside_workspace(&path, ctx).is_none() {
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
