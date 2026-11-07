//! The start of a user's turn: routing the request, loading an invoked skill,
//! and putting the user's message in the conversation. A turn that job
//! completions start (`wake`) has none of this.

use super::{Agent, Event, Events};
use crate::llm::Message;
use crate::tools::Group;
use anyhow::Result;

impl Agent {
    pub async fn turn(&mut self, text: &str, tx: &Events) -> Result<()> {
        self.run_turn(Some(text), None, tx).await
    }

    /// A turn the user started with `/<skill> text`: the skill's SKILL.md (as
    /// the `skill` tool returns it) goes into the user message ahead of
    /// `text`, and an `invoke` event records which skill it was. A skill that
    /// is gone by now is an error and no turn.
    pub async fn invoke(&mut self, skill: &str, text: &str, tx: &Events) -> Result<()> {
        self.invoke_as(skill, text, &format!("/{skill} {text}"), tx).await.map(|_| ())
    }

    /// Preserve the exact submitted slash line separately from skill injection.
    /// False when no turn ran: the skill was gone, which is only an `Error`.
    pub async fn invoke_as(&mut self, skill: &str, text: &str, original: &str, tx: &Events) -> Result<bool> {
        match crate::tools::skill::load(&self.ctx, skill, None) {
            Ok(body) => self.run_turn(Some(text), Some((skill, body, original.to_string())), tx).await.map(|()| true),
            Err(e) => {
                self.journal.emit(Event::Error { message: e }, tx);
                Ok(false)
            }
        }
    }

    pub(super) async fn begin_user_turn(&mut self, text: &str, invoked: Option<(&str, String, String)>, tx: &Events) {
        let original = match &invoked {
            Some((_, _, original)) => original.clone(),
            None => text.to_string(),
        };
        self.journal.emit(Event::Submitted { text: original }, tx);
        self.enable(self.groups.clone(), tx).await;
        let mut route = self.route(text).await;
        // Project skills are listed in the system prompt, so the tool that
        // loads them is on wherever they are: being in the repo is the trigger.
        let has_kit_skills = self.ctx.project_skills.as_ref().is_some_and(|p| !p.list().is_empty());
        if has_kit_skills && !route.groups.contains(&Group::Skills) {
            route.groups.push(Group::Skills);
        }
        if let Some((name, _, _)) = &invoked {
            route.skills.retain(|(n, _)| n != name);
            // Its linked files are read with the skill tool.
            if !route.groups.contains(&Group::Skills) {
                route.groups.push(Group::Skills);
            }
        }
        self.journal.emit(
            Event::Route {
                groups: route.groups.iter().map(Group::label).collect(),
                skills: route.skills.iter().map(|(n, p)| format!("{n} {p:.2}")).collect(),
                source: route.source.clone(),
                ms: route.ms,
                note: route.note.clone(),
            },
            tx,
        );
        self.enable(route.groups.clone(), tx).await;

        for (name, _) in &route.skills {
            if let Some(lib) = self.ctx.skill_lib(name) {
                lib.suggested(name);
            }
        }
        let mut content = text.to_string();
        if let Some((name, body, _)) = invoked {
            let scope = if self.ctx.project_skills.as_ref().is_some_and(|p| p.get(name).is_some()) {
                "project"
            } else {
                "global"
            };
            self.journal.emit(Event::Invoke { skill: name.to_string(), scope: scope.to_string() }, tx);
            content = crate::prompt::invoked_skill(name, &body, text);
        }
        if let Some(r) = crate::prompt::skill_reminder(&route.skills, &self.ctx.all_skills()) {
            content.push_str("\n\n");
            content.push_str(&r);
        }
        self.push(Message::user(content), tx);
        self.user_turns += 1;
        self.turns_since_memory += 1;
    }
}
