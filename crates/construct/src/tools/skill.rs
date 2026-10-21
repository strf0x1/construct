//! skill (load a procedure) and skill_manage (write the library). Both work
//! on the global library and, inside a repo, its project kit.

use super::{Ctx, Display, Group, Output, Tool, ToolError, def, obj};
use serde_json::{Value, json};

pub struct SkillView;

impl SkillView {
    pub const NAME: &str = "skill";
}

#[async_trait::async_trait]
impl Tool for SkillView {
    fn name(&self) -> &'static str {
        Self::NAME
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
        let n = a["name"].as_str().ok_or("name is required")?;
        let r = match ctx.project_skills.as_ref().filter(|p| p.get(n).is_some()) {
            Some(p) => p.view(n, a["file"].as_str()),
            None => ctx.skills.view(n, a["file"].as_str()),
        };
        r.map(Output::plain).map_err(ToolError::from)
    }
}

pub struct SkillManage;

impl SkillManage {
    pub const NAME: &str = "skill_manage";
}

#[async_trait::async_trait]
impl Tool for SkillManage {
    fn name(&self) -> &'static str {
        Self::NAME
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
    // An existing skill is edited where it lives; a new one goes where `scope` says.
    let project = ctx.project_skills.as_ref();
    let lib = match (s("scope"), s("name").and_then(|n| project.filter(|p| p.get(n).is_some()))) {
        (_, Some(p)) => p,
        (Some("project"), None) => match (project, &ctx.memory.project) {
            (Some(p), Some(pr)) => {
                pr.touch();
                p
            }
            _ => return Err("not in a git repo, so there is no project kit; use scope 'global'".into()),
        },
        _ => &ctx.skills,
    };
    match s("action").unwrap_or("") {
        "list" => {
            let mut lines: Vec<String> = project
                .map(|p| p.list().iter().map(|k| format!("- {} (project): {}", k.name, k.description)).collect())
                .unwrap_or_default();
            lines.extend(ctx.skills.list().iter().map(|k| format!("- {}: {}", k.name, k.description)));
            if lines.is_empty() {
                return Ok("the skill library is empty".into());
            }
            Ok(lines.join("\n"))
        }
        "create" => lib.create(name()?, s("content").ok_or("content is required")?, "agent"),
        "edit" => lib.edit(name()?, s("content").ok_or("content is required")?),
        "patch" => lib.patch(
            name()?,
            s("file"),
            s("old_string").ok_or("old_string is required")?,
            s("new_string").ok_or("new_string is required")?,
        ),
        "write_file" => lib.write_file(name()?, s("file").ok_or("file is required")?, s("content").unwrap_or("")),
        "remove_file" => lib.remove_file(name()?, s("file").ok_or("file is required")?),
        "delete" => lib.archive(name()?, s("absorbed_into")),
        other => Err(format!("unknown action {other:?}")),
    }
}
