//! The `Skills` source's promise: `skill` and `skill_manage` in their groups
//! under the tool contract, named for the sandbox's denials and the prompt
//! only when the source is there, and loading a skill as `/<skill>` does.

use construct_core::config::ToolsConfig;
use construct_core::permission::PolicyTools;
use construct_core::tools::contract::{self, Scratch};
use construct_core::tools::{Catalog, Deck, Group, ToolSources};
use construct_tools_skills::Skills;
use serde_json::json;

const SKILL: &str = "---\nname: tidy\ndescription: Tidy a repo\n---\n\n1. Sweep\n";

#[tokio::test(flavor = "multi_thread")]
async fn the_skill_tools_keep_the_tool_contract_and_load_as_a_user_does() {
    let s = Scratch::new("skills-source");
    let sources = ToolSources::default().with(Skills);
    let c = Catalog::build_extended(&ToolsConfig::default(), None, &Deck::default(), &sources).unwrap();
    contract::holds(&c, "skills-contract").await;
    assert_eq!((c.group_of("skill"), c.group_of("skill_manage")), (Some(Group::Skills), Some(Group::SkillWrite)));
    s.call(&c, "skill_manage", &json!({ "action": "create", "name": "tidy", "content": SKILL }).to_string())
        .await
        .unwrap();
    let tool = s.call(&c, "skill", &json!({ "name": "tidy" }).to_string()).await.unwrap().text;
    assert_eq!(tool, s.load_skill("tidy", None).unwrap(), "the tool and `/<skill>` load the same text");
    assert!(tool.starts_with("[global: ") && tool.contains("1. Sweep"), "{tool}");
}

#[test]
fn the_names_come_with_the_source() {
    let mut tools = PolicyTools::default();
    ToolSources::default().policy_tools(&mut tools);
    assert_eq!((tools.skill_view, tools.skill_manage), (None, None));
    ToolSources::default().with(Skills).policy_tools(&mut tools);
    assert_eq!((tools.skill_view, tools.skill_manage), (Some("skill"), Some("skill_manage")));
}
