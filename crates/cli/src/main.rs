//! `papers` executable. See `cm_cli` and docs/ADR/cli.md.
fn main() {
    // Not `stdout().lock()`: `papers mcp` hands stdout to the MCP server, which writes from its
    // own thread; a lock held here for the whole run would deadlock it.
    let code = cm_cli::run(
        std::env::args_os(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        &mut cm_cli::TerminalPrompt,
    );
    std::process::exit(code);
}
