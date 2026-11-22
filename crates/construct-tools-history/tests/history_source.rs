//! The `History` source's promise: both tools in the history group, in the
//! order they had inside core, and `journal_query` named for the sandbox's
//! denials only when the source is there.

use construct_core::config::ToolsConfig;
use construct_core::permission::PolicyTools;
use construct_core::tools::{Catalog, Deck, Group, ToolSources};
use construct_tools_history::History;

#[test]
fn both_tools_join_the_history_group() {
    let sources = ToolSources::default().with(History);
    let c = Catalog::build_extended(&ToolsConfig::default(), None, &Deck::default(), &sources).unwrap();
    let names: Vec<String> =
        c.definitions(&[Group::History]).iter().map(|d| d["function"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["recall_result", "journal_query"]);
    assert!(
        !Catalog::build(&ToolsConfig::default(), None).unwrap().groups().contains(&Group::History),
        "core has none"
    );
}

#[test]
fn the_denials_name_journal_query_only_with_the_source() {
    let mut tools = PolicyTools::default();
    ToolSources::default().policy_tools(&mut tools);
    assert_eq!(tools.journal_query, None);
    ToolSources::default().with(History).policy_tools(&mut tools);
    assert_eq!(tools.journal_query, Some("journal_query"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tools_keep_the_tool_contract() {
    let sources = ToolSources::default().with(History);
    let c = Catalog::build_extended(&ToolsConfig::default(), None, &Deck::default(), &sources).unwrap();
    construct_core::tools::contract::holds(&c, "history-contract").await;
}
