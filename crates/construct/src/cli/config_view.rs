//! Print the effective config and MCP sandbox policies for `construct config`.

use construct_core::config::{Config, Paths};
use construct_core::{sandbox, secrets};
use std::path::Path;

pub(super) fn show(cfg: &Config, paths: &Paths, cwd: &Path) -> anyhow::Result<()> {
    println!("# {}\n{}", paths.config().display(), toml::to_string_pretty(cfg)?);
    for (key, source) in &cfg.provenance {
        println!("# {key} = {source}");
    }
    print!("{}", mcp_policies(cfg, paths, cwd));
    Ok(())
}

/// Each MCP server's effective sandbox, as comments under the config.
fn mcp_policies(cfg: &Config, paths: &Paths, cwd: &Path) -> String {
    let mut out = String::new();
    for (name, server) in &cfg.mcp {
        let p = sandbox::Policy::for_mcp(&cfg.sandbox, server, cwd, paths);
        // Building a policy makes its private temp dir; nothing runs here.
        let _ = std::fs::remove_dir(&p.tmp);
        let list = |v: &[std::path::PathBuf]| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
        let secrets: Vec<String> = server
            .secrets
            .iter()
            .map(|s| format!("{s} ({})", if secrets::optional(s).is_some() { "set" } else { "not set" }))
            .collect();
        out.push_str(&format!(
            "\n# mcp.{name} runs sandboxed{}\n#   read: {}\n#   write: {}\n#   plus system paths and toolchains, read-only\n#   network: {}\n#   secrets: {}\n",
            if p.off { " (mode = \"off\": no filesystem boundary)" } else { "" },
            list(&p.read),
            list(&p.write),
            if p.no_network { "denied" } else { "allowed" },
            if secrets.is_empty() { "none".into() } else { secrets.join(", ") },
        ));
    }
    out
}
