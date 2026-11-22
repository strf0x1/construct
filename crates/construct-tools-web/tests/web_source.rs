//! The `Web` source's promise to a catalog: both tools in the web group, in
//! the order they had inside core, a bad search backend refused at startup,
//! and `[tools] disabled` reaching them like any built-in.

use construct_core::config::ToolsConfig;
use construct_core::tools::{Catalog, Deck, Group, ToolSources};
use construct_tools_web::Web;

fn build(cfg: &ToolsConfig) -> anyhow::Result<Catalog> {
    Catalog::build_extended(cfg, None, &Deck::default(), &ToolSources::default().with(Web))
}

#[test]
fn both_tools_join_the_web_group() {
    let c = build(&ToolsConfig::default()).unwrap();
    let names: Vec<String> =
        c.definitions(&[Group::Web]).iter().map(|d| d["function"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["web_search", "web_fetch"]);
    assert!(c.groups().contains(&Group::Web));
    assert!(!Catalog::build(&ToolsConfig::default(), None).unwrap().groups().contains(&Group::Web), "core has none");
}

#[test]
fn the_web_search_backend_is_checked_at_startup() {
    let toml = |search: &str| -> ToolsConfig { toml::from_str(&format!("[web]\nsearch = \"{search}\"")).unwrap() };
    for ok in ["auto", "exa", "brave", "tavily", "ddg"] {
        assert!(build(&toml(ok)).is_ok(), "{ok}");
    }
    let err = build(&toml("google")).err().unwrap().to_string();
    assert!(err.contains("tools.web.search") && err.contains("\"google\"") && err.contains("tavily"), "{err}");
    assert_eq!(ToolsConfig::default().web.search, "auto");
}

#[test]
fn disabled_reaches_the_web_tools() {
    let cfg = ToolsConfig { disabled: vec!["web_search".into(), "web_fetch".into()], ..Default::default() };
    let c = build(&cfg).unwrap();
    assert!(c.get("web_search").is_none() && c.group_of("web_fetch").is_none());
    assert!(!c.groups().contains(&Group::Web));
    // A catalog built without sources (the learner's) accepts the names.
    assert!(Catalog::build_without_sources(&cfg).is_ok());
    assert!(Catalog::build(&cfg, None).is_err(), "a strict one without the source does not");
}
