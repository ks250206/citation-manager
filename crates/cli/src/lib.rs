//! `papers`: the GUI-less command-line adapter over papersd's authenticated IPC (#28).
//!
//! The CLI never opens the library database: it lazily starts the per-library `papersd`
//! (next to this executable) and sends `cm_protocol::ipc::Command`s. stdout carries one
//! `papers.v1` JSON envelope (or `--text`), stderr carries diagnostics, and exit codes are
//! semantic (`output::exit`). Credentials and requested capabilities live in `credentials`.
pub mod args;
pub mod commands;
pub mod connect;
pub mod credentials;
pub mod decide;
pub mod output;
pub mod paths;

use args::{Cli, Command};
use clap::Parser;
use output::{CliError, Format, emit, exit};
use std::{ffi::OsString, io::Write};

/// Interactive confirmation for writes when `--yes` is absent.
pub trait Prompt {
    /// `None`: no terminal to ask on (non-interactive).
    fn confirm(&mut self, question: &str) -> Option<bool>;
}

/// Asks on stderr/stdin only when both are terminals.
pub struct TerminalPrompt;
impl Prompt for TerminalPrompt {
    fn confirm(&mut self, question: &str) -> Option<bool> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
            return None;
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok()?;
        Some(matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
    }
}

/// Runs one invocation and returns the process exit code.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write, prompt: &mut dyn Prompt) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let to_stdout = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
            let rendered = e.render().to_string();
            if to_stdout {
                let _ = write!(out, "{rendered}");
                return exit::OK;
            }
            let _ = write!(err, "{rendered}");
            return exit::USAGE;
        }
    };
    let mut format = if cli.text { Format::Text } else { Format::Json };
    if let Command::Export(args) = &cli.command
        && args.output.is_none()
    {
        // stdout is the bibliography itself: failures are reported on stderr only.
        format = Format::Text;
    }
    match &cli.command {
        Command::Completions(args) => return commands::completions::write(args.shell, out, err),
        // stdout belongs to the MCP protocol: never print an envelope there.
        Command::Mcp => return commands::mcp::run(err),
        _ => {}
    }
    let name = commands::name(&cli.command);
    let result = commands::dispatch(&cli, out, err, prompt);
    match result {
        // Export to stdout already wrote the bibliography itself.
        Ok(commands::Outcome::Raw) => exit::OK,
        Ok(commands::Outcome::Report(report)) => emit(format, &name, Ok(report), out, err),
        Err(e) => emit(format, &name, Err::<output::Report, CliError>(e), out, err),
    }
}
