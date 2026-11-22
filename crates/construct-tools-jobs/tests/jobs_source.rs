//! The `Jobs` source's promise: the `job` tool in the jobs group, which a
//! catalog without the source does not have.

use construct_core::config::ToolsConfig;
use construct_core::tools::{Catalog, Deck, Group, ToolSources};
use construct_tools_jobs::Jobs;

#[test]
fn the_job_tool_joins_the_jobs_group() {
    let sources = ToolSources::default().with(Jobs);
    let c = Catalog::build_extended(&ToolsConfig::default(), None, &Deck::default(), &sources).unwrap();
    let names: Vec<String> =
        c.definitions(&[Group::Jobs]).iter().map(|d| d["function"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["job"]);
    assert!(!Catalog::build(&ToolsConfig::default(), None).unwrap().groups().contains(&Group::Jobs), "core has none");
}
