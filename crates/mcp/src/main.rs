//! `papers-mcp`: the MCP stdio server executable (#29). `papers mcp ARGS` (#28) runs the same
//! `cm_mcp::main(ARGS)`. Installed next to papersd, which it starts on demand.
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(cm_mcp::main(std::env::args_os().skip(1).collect()))
}
