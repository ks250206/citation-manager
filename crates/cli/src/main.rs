//! `papers` executable. See `cm_cli` and docs/ADR/cli.md.
fn main() {
    let stdout = std::io::stdout();
    let code = cm_cli::run(
        std::env::args_os(),
        &mut stdout.lock(),
        &mut std::io::stderr(),
        &mut cm_cli::TerminalPrompt,
    );
    std::process::exit(code);
}
