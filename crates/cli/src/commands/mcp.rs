//! `papers mcp`: serves MCP (#29) over this process's stdin/stdout for the library the CLI
//! resolved (`--library`/`PAPERS_LIBRARY`, `--runtime-dir`, `--credential-file`, `--no-start`).
//! The server is read-only by default and never presents the owner's desktop credential: without
//! `--credential-file` it uses the default MCP credential file. Nothing but protocol frames is
//! written to stdout; diagnostics go to stderr.
use crate::{
    args::Cli,
    commands,
    output::{CliError, exit},
};
use std::io::Write;

pub fn run(cli: &Cli, err: &mut dyn Write) -> i32 {
    match config(cli) {
        Ok(config) => match cm_mcp::run_stdio_with(config) {
            Ok(()) => exit::OK,
            Err(cm_mcp::StdioError::Usage(reason)) => fail(err, exit::USAGE, &reason),
            Err(e) => fail(err, exit::UNAVAILABLE, &e.to_string()),
        },
        Err(e) => fail(err, e.kind.exit_code(), &e.message),
    }
}

fn config(cli: &Cli) -> Result<cm_mcp::StdioConfig, CliError> {
    let connector = commands::target(cli)?.mcp_connector()?;
    Ok(cm_mcp::StdioConfig::from_connector(&connector))
}

fn fail(err: &mut dyn Write, code: i32, reason: &str) -> i32 {
    let _ = writeln!(err, "papers: error: mcp: {reason}");
    code
}
