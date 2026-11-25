//! `papers mcp`: hands stdio to the stable `cm_mcp::run_stdio` entry point (#7). Until #29
//! implements the server, it reports that MCP is unavailable on stderr and exits 7. Nothing is
//! ever written to stdout here: it belongs to the MCP protocol.
use crate::output::exit;
use std::io::Write;

pub fn run(err: &mut dyn Write) -> i32 {
    match cm_mcp::run_stdio() {
        Ok(()) => exit::OK,
        Err(e) => {
            let _ = writeln!(
                err,
                "papers: error: MCP is not available: {e}. The MCP feature cannot be used yet; \
                 the other papers commands work without it."
            );
            exit::UNAVAILABLE
        }
    }
}
