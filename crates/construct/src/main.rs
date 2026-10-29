//! The binary: the sandbox helper re-exec (`construct __sandbox`) and
//! `cli::run`. Everything testable lives in the library.
use anyhow::Result;
use construct::{cli, sandbox, trace};

fn main() -> Result<()> {
    // The sandbox helper must stay single-threaded and do nothing else.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("__sandbox") {
        sandbox::exec_child(&argv[2..]);
    }
    trace::mark("main");
    cli::run()
}
