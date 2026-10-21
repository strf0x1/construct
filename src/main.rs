use anyhow::Result;
use construct::{cli, sandbox, tui};

fn main() -> Result<()> {
    // The sandbox helper must stay single-threaded and do nothing else.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("__sandbox") {
        sandbox::exec_child(&argv[2..]);
    }
    tui::trace("main");
    cli::run()
}
