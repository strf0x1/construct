//! skill (load a procedure) and skill_manage (write the library). Both work
//! on the global library and, inside a repo, its project kit.
//!
//! An optional tool crate (#271, ADR 0064): the binary registers `Skills` as
//! a `ToolSource` when its `skills` feature is on. The library itself, the
//! loader a user's `/<skill>` shares (`skills::Stack::load`) and the wire names
//! (`skills::VIEW_TOOL`, `MANAGE_TOOL`) stay in core, which the learning
//! pass and the prompt need either way.

use construct_core::config::ToolsConfig;
use construct_core::permission::PolicyTools;
use construct_core::skills::{MANAGE_TOOL, VIEW_TOOL};
use construct_core::tools::{Ctx, Deck, Display, Group, Output, Tool, ToolError, ToolKind, ToolSource, def, obj};
use serde_json::{Value, json};
use std::sync::Arc;

/// What the composition root registers: both skill tools, in the order they
/// had inside core, named for the sandbox's denials and the prompt.
pub struct Skills;

impl ToolSource for Skills {
    fn tools(
        &self,
        _cfg: &ToolsConfig,
        _deck: &Deck,
        _jobs: &construct_core::jobs::Jobs,
    ) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
        Ok(vec![Arc::new(SkillView), Arc::new(SkillManage)])
    }

    fn policy_tools(&self, tools: &mut PolicyTools) {
        tools.skill_view = Some(SkillView::NAME);
        tools.skill_manage = Some(SkillManage::NAME);
    }
}

pub struct SkillView;

impl SkillView {
    pub const NAME: &str = VIEW_TOOL;
}

#[async_trait::async_trait]
impl Tool for SkillView {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn group(&self) -> Group {
        Group::Skills
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Load a skill: a procedure written down from past work. Returns its SKILL.md and linked files; pass file to read one of those.",
            obj(json!({ "name": { "type": "string" }, "file": { "type": "string" } }), &["name"]),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "name", preview_lines: 0 }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        if let Some(k) = a.as_object().and_then(|o| o.keys().find(|k| !["name", "file"].contains(&k.as_str()))) {
            return Err(format!(
                "{} only loads a skill and takes no {k:?}; use {} to list, create or edit",
                Self::NAME,
                SkillManage::NAME
            )
            .into());
        }
        let Some(n) = a["name"].as_str() else {
            return Err(
                format!("name is required; {} with action=list shows the available skills", SkillManage::NAME).into()
            );
        };
        Ok(Output::plain(ctx.skills().load(n, a["file"].as_str())?))
    }
}

pub struct SkillManage;

impl SkillManage {
    pub const NAME: &str = MANAGE_TOOL;
}

#[async_trait::async_trait]
impl Tool for SkillManage {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    fn group(&self) -> Group {
        Group::SkillWrite
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Write the skill library. create (content = full SKILL.md with frontmatter name/description), patch (old_string/new_string, optional file), edit (full rewrite), write_file (file under references/, templates/ or scripts/), remove_file, delete (archives; absorbed_into names the skill that took over), list. Skills are class-level procedures: trigger, numbered steps with exact commands, pitfalls, how to verify. The description is how the skill gets found later: models deciding whether a skill fits their task see only its name and description, so say plainly what situations it is for (and what it isn't, if a similar skill exists) and keep the steps in the body.",
            obj(
                json!({
                    "action": { "type": "string", "enum": ["create", "patch", "edit", "write_file", "remove_file", "delete", "list"] },
                    "name": { "type": "string" },
                    "content": { "type": "string" },
                    "file": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "absorbed_into": { "type": "string" },
                    "scope": { "type": "string", "enum": ["global", "project"], "description": "For create: 'project' when the procedure only makes sense in the current repo (its build, landmines, conventions); 'global' for methods that work across repos. Default global." }
                }),
                &["action"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "action", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        let action = a.get("action").and_then(Value::as_str).unwrap_or("").to_string();
        let out = manage(ctx, &a).map_err(ToolError::from)?;
        // A listing changes nothing; the learning pass doesn't count it as an action.
        Ok(Output::plain(out).with_data(json!({ "action": action, "changed": action != "list" })))
    }
}

fn manage(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str);
    let name = || s("name").ok_or_else(|| "name is required".to_string());
    if s("action") == Some("list") {
        let lines: Vec<String> = ctx
            .skills()
            .libs()
            .into_iter()
            .flat_map(|lib| {
                lib.list()
                    .into_iter()
                    .map(move |k| format!("- {} ({}): {}", k.name, lib.locate(&k.name), k.description))
            })
            .collect();
        if lines.is_empty() {
            return Ok("the skill library is empty".into());
        }
        return Ok(lines.join("\n"));
    }
    // An existing skill is edited where it lives; a new one goes where `scope` says.
    let lib = ctx.skills().writable(s("name"), s("scope"))?;
    match s("action").unwrap_or("") {
        "create" => lib.create(name()?, s("content").ok_or("content is required")?, "agent"),
        "edit" => lib.edit(name()?, s("content").ok_or("content is required")?),
        "patch" => {
            let old = s("old_string").ok_or("old_string is required")?;
            let new = s("new_string").ok_or("new_string is required")?;
            lib.patch(name()?, s("file"), new, |text| {
                construct_core::tools::edit::replace::replace_once(text, old, new, false).map_err(|e| e.message)
            })
        }
        "write_file" => lib.write_file(name()?, s("file").ok_or("file is required")?, s("content").unwrap_or("")),
        "remove_file" => lib.remove_file(name()?, s("file").ok_or("file is required")?),
        "delete" => lib.archive(name()?, s("absorbed_into")),
        other => Err(format!("unknown action {other:?}")),
    }
    .map(|msg| match (s("action"), s("name")) {
        (Some("create" | "edit" | "patch" | "write_file" | "remove_file"), Some(n)) => {
            format!("{msg} ({})", lib.locate(n))
        }
        _ => msg,
    })
}
