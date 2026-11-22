//! The local router: a lexical fallback for when Jev has no key, is slow or
//! is down. Keyword lists per group and word overlap for skills and MCP
//! servers; crude, fast, and never a network call.

use super::{Input, Route, overlap, skill_text};
use crate::config::RouterConfig;
use crate::tools::Group;

fn any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

pub(super) fn local(cfg: &RouterConfig, input: &Input<'_>) -> Route {
    let _ = cfg;
    let t = input.request.to_lowercase();
    let mut groups = Vec::new();
    if any(
        &t,
        &[
            "http://",
            "https://",
            "www.",
            "search the web",
            "web search",
            "google",
            "look up",
            "lookup",
            "online",
            "internet",
            "website",
            "docs for",
            "documentation",
            "latest version",
            "release notes",
            "news",
            "research",
        ],
    ) {
        groups.push(Group::Web);
    }
    if any(
        &t,
        &[
            "remember",
            "forget",
            "i prefer",
            "i like",
            "i hate",
            "i don't like",
            "always ",
            "never ",
            "call me",
            "my name",
            "from now on",
            "stop doing",
            "don't ever",
        ],
    ) {
        groups.push(Group::Memory);
    }
    if any(
        &t,
        &[
            "last time",
            "previous session",
            "past session",
            "earlier session",
            "we did",
            "yesterday",
            "last week",
            "before, we",
            "remember when",
            "other session",
            "you said",
        ],
    ) {
        groups.push(Group::History);
    }
    if t.contains("skill") && any(&t, &["save", "write", "create", "update", "make", "delete", "add"]) {
        groups.push(Group::SkillWrite);
    }
    for (name, desc) in input.mcp {
        if t.contains(&name.to_lowercase()) || overlap(input.request, desc) >= 0.34 {
            groups.push(Group::Mcp(name.clone()));
        }
    }
    let mut ranked: Vec<(String, f32)> = input
        .skills
        .iter()
        .map(|s| {
            let named = t.contains(&s.name) || t.contains(&s.name.replace('-', " "));
            (s.name.clone(), if named { 1.0 } else { overlap(input.request, &skill_text(s)) })
        })
        .filter(|(_, p)| *p >= 0.25)
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(3);
    if !ranked.is_empty() || groups.contains(&Group::SkillWrite) {
        groups.push(Group::Skills);
    }
    Route { groups, skills: ranked, source: "local".into(), ms: 0, note: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Skill;
    use std::collections::BTreeMap;

    #[test]
    fn local_routes_obvious_cases() {
        let cfg = RouterConfig::default();
        let skills = vec![Skill {
            name: "rust-release".into(),
            description: "Cut a release of a Rust crate: bump version, changelog, tag, publish".into(),
            dir: Default::default(),
        }];
        let mcp = BTreeMap::new();
        let groups = [Group::Web, Group::Memory, Group::History, Group::SkillWrite, Group::Skills].into();
        let r = local(
            &cfg,
            &Input {
                request: "publish a new release of this crate with a changelog",
                previous: "",
                skills: &skills,
                mcp: &mcp,
                groups: &groups,
            },
        );
        assert_eq!(r.skills.first().map(|s| s.0.as_str()), Some("rust-release"));
        let r = local(
            &cfg,
            &Input {
                request: "what's in https://example.com",
                previous: "",
                skills: &skills,
                mcp: &mcp,
                groups: &groups,
            },
        );
        assert!(r.groups.contains(&Group::Web));
        assert!(r.skills.is_empty());
        let r = local(
            &cfg,
            &Input { request: "remember that I prefer tabs", previous: "", skills: &[], mcp: &mcp, groups: &groups },
        );
        assert!(r.groups.contains(&Group::Memory));
    }
}
