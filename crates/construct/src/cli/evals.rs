//! `construct route` and `construct gate`: the hidden commands the router and
//! learning-gate evals drive (`bench/jev/eval.py`). Each reads JSON lines on
//! stdin and prints one JSON line per input, so an eval is a pipe.

use crate::config::{Config, Paths};
use crate::{learn, router, skills, tools};
use anyhow::Result;
use std::io::Read;

fn stdin_lines() -> Vec<serde_json::Value> {
    let mut text = String::new();
    let _ = std::io::stdin().read_to_string(&mut text);
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

pub(super) async fn route(cfg: Config, paths: Paths) -> Result<()> {
    let skills = skills::Library::new(&paths).list();
    let mcp: std::collections::BTreeMap<String, String> =
        cfg.mcp.iter().map(|(k, v)| (k.clone(), v.description.clone())).collect();
    let http = reqwest::Client::builder().tcp_nodelay(true).build()?;
    for v in stdin_lines() {
        let request = v["request"].as_str().unwrap_or("");
        let previous = v["previous"].as_str().unwrap_or("");
        let r =
            router::route(&cfg.router, &http, router::Input { request, previous, skills: &skills, mcp: &mcp }).await;
        let out = serde_json::json!({
            "request": request,
            "groups": r.groups.iter().map(tools::Group::label).collect::<Vec<_>>(),
            "skills": r.skills,
            "source": r.source,
            "ms": r.ms,
            "note": r.note,
        });
        println!("{out}");
    }
    Ok(())
}

pub(super) async fn gate(cfg: Config) -> Result<()> {
    let strs = |v: &serde_json::Value| -> Vec<String> {
        v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
    };
    for v in stdin_lines() {
        let started = std::time::Instant::now();
        let g = learn::gate_parts(&cfg, &strs(&v["users"]), &strs(&v["errors"]), &strs(&v["loaded"])).await;
        let out = serde_json::json!({ "kind": g.kind.map(|k| format!("{k:?}")), "source": g.source, "note": g.note, "ms": started.elapsed().as_millis() as u64 });
        println!("{out}");
    }
    Ok(())
}
