//! Contract: every core tool says what it does in ACP's vocabulary, so an
//! editor's icons are right and a new tool that forgets gets `other`, not a
//! wrong guess.

use construct::config::ToolsConfig;
use construct::tools::{Catalog, ToolKind};

#[test]
fn core_tools_map_to_the_acp_categories() {
    let cat = Catalog::build(&ToolsConfig::default(), None).unwrap();
    for (tool, kind) in [
        ("read", ToolKind::Read),
        ("edit", ToolKind::Edit),
        ("write", ToolKind::Edit),
        ("grep", ToolKind::Search),
        ("glob", ToolKind::Search),
        ("shell", ToolKind::Execute),
    ] {
        assert_eq!(cat.kind_of(tool), kind, "{tool}");
    }
    assert_eq!(cat.kind_of("no_such_tool"), ToolKind::Other);
}
