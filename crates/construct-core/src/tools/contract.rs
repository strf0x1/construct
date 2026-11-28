//! The promises every tool makes to a catalog, as checks a tool crate's own
//! tests run on its tools (#271): a well-formed function schema, and missing
//! arguments refused in prose that names the first required one.
//!
//! The design choice: these live in core behind `test-support`, not in one
//! crate's tests, so a tool moved out of core, or a replacement for one,
//! is held to the same contract without a copy of it.

use super::{Catalog, Ctx, Group, Output, ToolError};
use crate::config::{MemoryConfig, Paths, SandboxConfig};
use serde_json::json;
use std::path::PathBuf;

/// A throwaway home and work dir under the system temp dir, and a `Ctx` on
/// them with the default sandbox; removed when dropped.
pub struct Scratch {
    root: PathBuf,
    ctx: Ctx,
}

impl Scratch {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("construct-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        if let Err(e) = std::fs::create_dir_all(&work) {
            panic!("scratch dir {}: {e}", work.display());
        }
        let paths = Paths { home: root.join("home") };
        let memory = crate::memory::Store::new(&paths, &MemoryConfig::default());
        let sandbox = crate::sandbox::Policy::new(&SandboxConfig::default(), &work, &paths);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        Self { root, ctx: Ctx::new(work, paths, memory, rx, sandbox) }
    }

    pub async fn call(&self, catalog: &Catalog, name: &str, args: &str) -> Result<Output, ToolError> {
        catalog.call(&self.ctx, name, args).await
    }

    pub fn load_skill(&self, name: &str, file: Option<&str>) -> Result<String, String> {
        self.ctx.skills().load(name, file)
    }

    async fn check_missing_arguments(&self, catalog: &Catalog, groups: &[Group]) {
        missing_arguments_are_refused(catalog, &self.ctx, groups).await;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Both checks over every group, for a tool crate's catalog.
pub async fn holds(catalog: &Catalog, name: &str) {
    let s = Scratch::new(name);
    schemas_are_well_formed(catalog, &all_groups());
    s.check_missing_arguments(catalog, &all_groups()).await;
}

/// Every definition in `groups` is an OpenAI function with a description,
/// an object of typed properties, and required keys that are properties.
pub fn schemas_are_well_formed(catalog: &Catalog, groups: &[Group]) {
    let defs = catalog.definitions(groups);
    assert_eq!(defs.len(), catalog.names().len(), "every tool belongs to one of {groups:?}");
    for (d, name) in defs.iter().zip(catalog.names()) {
        assert_eq!(d["type"], "function");
        assert_eq!(d["function"]["name"], name, "definition order follows the catalog");
        assert!(!d["function"]["description"].as_str().unwrap_or("").is_empty(), "{name} has a description");
        let params = &d["function"]["parameters"];
        assert_eq!(params["type"], "object", "{name}");
        let props = params["properties"].as_object().unwrap_or_else(|| panic!("{name} has properties"));
        for r in params["required"].as_array().into_iter().flatten() {
            assert!(r.as_str().is_some_and(|r| props.contains_key(r)), "{name}: required {r} is a property");
        }
        for (k, p) in props {
            assert!(p.get("type").is_some(), "{name}.{k} has a type");
        }
    }
}

/// A call with no arguments to any tool in `groups` fails with prose that
/// names its first required argument.
pub async fn missing_arguments_are_refused(catalog: &Catalog, ctx: &Ctx, groups: &[Group]) {
    for d in catalog.definitions(groups) {
        let name = d["function"]["name"].as_str().unwrap_or_default();
        let first = d["function"]["parameters"]["required"][0].as_str().unwrap_or_default();
        match catalog.call(ctx, name, &json!({}).to_string()).await {
            Ok(o) => panic!("{name} ran with no arguments: {}", o.text),
            Err(e) => assert!(e.message.contains(first), "{name}: {:?} should name {first}", e.message),
        }
    }
}

/// The groups a tool can be in, for a check over a whole catalog.
pub fn all_groups() -> Vec<Group> {
    let mut g = vec![Group::Core, Group::Web, Group::Memory, Group::Skills, Group::SkillWrite, Group::History];
    g.extend([Group::Jobs, Group::Developer, Group::Peers, Group::Boundary, Group::Coordinator]);
    g
}
