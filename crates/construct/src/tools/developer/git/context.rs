//! Commit mutations share only the captured object and reference stores.
//! A private git directory owns all configuration, so configured hooks in the
//! repository or host config cannot turn an approved commit into host code.

use super::super::runner::{self, Executable, Scratch};
use super::{Commit, options};
use crate::tools::{Ctx, ToolError};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) struct CommitContext {
    _scratch: Scratch,
    work: PathBuf,
}

pub(super) struct Layout<'a> {
    pub branch: &'a str,
    pub common: &'a str,
    pub git_dir: &'a str,
    pub objects: &'a Path,
    pub refs: &'a str,
    pub sha256: bool,
}

fn bounded(root: &Path, path: &Path) -> Result<(), ToolError> {
    let relative = path.strip_prefix(root).map_err(|_| "git backing path is outside captured metadata")?;
    let parts: Vec<_> = relative.components().collect();
    let mut current = root.to_path_buf();
    for (position, component) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = component else { return Err("invalid git backing path".into()) };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("git backing storage must not contain symlinks".into());
                }
                if position + 1 < parts.len() && !metadata.is_dir() {
                    return Err("git backing path crosses a non-directory".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err("cannot inspect git backing storage".into()),
        }
    }
    if !crate::sandbox::Policy::real(path).starts_with(root) {
        return Err("git backing storage escapes captured metadata".into());
    }
    Ok(())
}

fn validate(ctx: &Ctx, layout: &Layout<'_>) -> Result<(), ToolError> {
    let common = Path::new(layout.common);
    let git_dir = Path::new(layout.git_dir);
    for root in [common, git_dir] {
        if root.canonicalize().map_err(|_| "captured git directory is unavailable")? != root
            || crate::permission::protected(root)
            || root.starts_with(crate::sandbox::Policy::real(&ctx.paths.home))
        {
            return Err("captured git directory changed or points into protected storage".into());
        }
    }
    if layout.refs == "files" {
        bounded(common, &common.join(layout.branch))?;
    } else {
        // Reftable deliberately stores a non-directory sentinel at refs/heads.
        // Its writable reference data lives in the reftable directory instead.
        bounded(common, &common.join("refs/heads"))?;
    }
    for path in [
        common.join("packed-refs"),
        common.join("reftable"),
        common.join("logs").join(layout.branch),
        common.join("objects"),
    ] {
        bounded(common, &path)?;
    }
    bounded(git_dir, &git_dir.join("logs/HEAD"))?;
    bounded(git_dir, &git_dir.join("index"))?;
    if layout.objects != common.join("objects") {
        return Err("object storage must stay inside captured common metadata".into());
    }
    for entry in std::fs::read_dir(layout.objects).map_err(|_| "cannot inspect object storage")? {
        let entry = entry.map_err(|_| "cannot inspect object storage entry")?;
        if entry.file_type().map_err(|_| "cannot inspect object storage entry")?.is_symlink() {
            return Err("object storage entries must not be symlinks".into());
        }
    }
    Ok(())
}

impl CommitContext {
    pub(super) fn new(ctx: &Ctx, plan: &Commit) -> Result<Self, ToolError> {
        Self::create(
            ctx,
            &Layout {
                branch: &plan.branch,
                common: &plan.common_dir,
                git_dir: &plan.git_dir,
                objects: Path::new(&plan.objects),
                refs: &plan.ref_format,
                sha256: plan.tree.len() == 64,
            },
            true,
        )
    }

    pub(super) fn index(
        ctx: &Ctx,
        layout: &Layout<'_>,
        index: &Path,
        shared: Option<&Path>,
    ) -> Result<Self, ToolError> {
        let context = Self::create(ctx, layout, false)?;
        for source in std::iter::once(index).chain(shared) {
            let root =
                if source.starts_with(layout.git_dir) { Path::new(layout.git_dir) } else { Path::new(layout.common) };
            bounded(root, source)?;
            let real = source.canonicalize().map_err(|_| "cannot read staged index")?;
            if crate::permission::protected(&real) || real.starts_with(&ctx.paths.home) {
                return Err("index storage points into a protected directory".into());
            }
            let name = if source == index {
                "index".as_ref()
            } else {
                source
                    .file_name()
                    .filter(|name| name.to_str().is_some_and(|name| name.starts_with("sharedindex.")))
                    .ok_or("unsupported split index storage")?
            };
            std::fs::copy(&real, context.path().join(name))
                .map_err(|_| "cannot copy staged index for private review")?;
            if source != index {
                std::fs::copy(&real, context._scratch.0.join("common").join(name))
                    .map_err(|_| "cannot copy split index backing storage")?;
            }
        }
        Ok(context)
    }

    fn create(ctx: &Ctx, layout: &Layout<'_>, publish: bool) -> Result<Self, ToolError> {
        use std::os::unix::fs::symlink;
        validate(ctx, layout)?;
        let scratch = Scratch::new(ctx)?;
        let root = scratch.0.join("common");
        let work = scratch.0.join("worktree");
        std::fs::create_dir(&root)
            .and_then(|_| std::fs::create_dir(&work))
            .map_err(|_| "cannot create private git layout")?;
        std::fs::write(work.join("commondir"), "../common\n").map_err(|_| "cannot prepare private common directory")?;
        let common = Path::new(layout.common);
        symlink(layout.objects, root.join("objects")).map_err(|_| "cannot link approved object storage")?;
        symlink(common.join("refs"), root.join("refs")).map_err(|_| "cannot link approved reference storage")?;
        symlink(common.join("packed-refs"), root.join("packed-refs")).map_err(|_| "cannot link packed references")?;
        if layout.refs == "reftable" {
            symlink(common.join("reftable"), root.join("reftable"))
                .map_err(|_| "cannot link approved reftable storage")?;
        }
        if publish {
            std::fs::create_dir_all(common.join("logs")).map_err(|_| "cannot prepare approved reference logs")?;
            symlink(common.join("logs"), root.join("logs")).map_err(|_| "cannot link approved reference logs")?;
            let logs = Path::new(layout.git_dir).join("logs");
            std::fs::create_dir_all(&logs).map_err(|_| "cannot prepare approved worktree logs")?;
            symlink(logs, work.join("logs")).map_err(|_| "cannot link approved worktree logs")?;
        }
        std::fs::write(work.join("HEAD"), format!("ref: {}\n", layout.branch))
            .map_err(|_| "cannot prepare frozen HEAD")?;
        let mut config = "[core]\nrepositoryformatversion = 1\nbare = true\nlogallrefupdates = true\n".to_owned();
        if layout.sha256 || layout.refs == "reftable" {
            config.push_str("[extensions]\n");
            if layout.sha256 {
                config.push_str("objectFormat = sha256\n");
            }
            if layout.refs == "reftable" {
                config.push_str("refStorage = reftable\n");
            }
        }
        std::fs::write(root.join("config"), config).map_err(|_| "cannot prepare isolated commit configuration")?;
        Ok(Self { _scratch: scratch, work })
    }

    pub(super) fn path(&self) -> &Path {
        &self.work
    }

    pub(super) async fn git(
        &self,
        ctx: &Ctx,
        exe: &Executable,
        args: &[&str],
        input: &[u8],
        extra: &[(&str, OsString)],
    ) -> Result<String, ToolError> {
        let mut command = options();
        command.extend(runner::args(args));
        let mut env = vec![
            ("GIT_DIR", self.path().as_os_str().to_owned()),
            ("GIT_INDEX_FILE", self.path().join("index").into_os_string()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ];
        env.extend_from_slice(extra);
        let output = Box::pin(runner::run(ctx, exe, self.path(), &command, input, &env)).await?;
        String::from_utf8(output).map(|s| s.trim_end().to_owned()).map_err(|_| "git returned non-UTF-8 metadata".into())
    }
}
