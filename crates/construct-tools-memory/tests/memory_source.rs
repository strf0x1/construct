//! The `MemoryTools` source's promise: the `memory` tool in the memory group
//! under the tool contract, named for the sandbox's denials only when the
//! source is there, and gone with its group when `[tools] disabled` says so.

use construct_core::config::ToolsConfig;
use construct_core::permission::PolicyTools;
use construct_core::tools::{Catalog, Deck, Group, ToolSources, contract};
use construct_tools_memory::MemoryTools;

fn build(cfg: &ToolsConfig) -> Catalog {
    let sources = ToolSources::default().with(MemoryTools);
    Catalog::build_extended(cfg, None, &Deck::default(), &sources).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_memory_tool_keeps_the_tool_contract() {
    contract::holds(&build(&ToolsConfig::default()), "memory-source").await;
}

#[test]
fn the_tool_joins_the_memory_group_and_leaves_with_it() {
    let c = build(&ToolsConfig::default());
    assert_eq!(c.group_of("memory"), Some(Group::Memory));
    assert!(!Catalog::build(&ToolsConfig::default(), None).unwrap().groups().contains(&Group::Memory), "core has none");
    let off = build(&ToolsConfig { disabled: vec!["memory".into()], ..Default::default() });
    assert_eq!(off.group_of("memory"), None, "a disabled tool can't switch its group on");
    let offered: Vec<String> = off.offerable(&Default::default()).into_iter().map(|(g, _)| g.label()).collect();
    assert!(!offered.contains(&"memory".to_string()), "an empty group isn't offered: {offered:?}");
    assert!(!off.groups().contains(&Group::Memory), "nor routed to");
}

#[test]
fn the_denials_name_memory_only_with_the_source() {
    let mut tools = PolicyTools::default();
    ToolSources::default().policy_tools(&mut tools);
    assert_eq!(tools.memory, None);
    ToolSources::default().with(MemoryTools).policy_tools(&mut tools);
    assert_eq!(tools.memory, Some("memory"));
}
