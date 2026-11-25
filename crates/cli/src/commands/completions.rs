//! Shell completion scripts generated from the clap grammar (clap_complete). The checked-in
//! copies under `crates/cli/completions/` are verified against this output by a test.
use crate::args::{Cli, Shell};
use clap::CommandFactory;
use std::io::Write;

pub fn generate(shell: Shell) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut command = Cli::command();
    match shell {
        Shell::Bash => clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut command,
            "papers",
            &mut buffer,
        ),
        Shell::Zsh => clap_complete::generate(
            clap_complete::Shell::Zsh,
            &mut command,
            "papers",
            &mut buffer,
        ),
        Shell::Fish => clap_complete::generate(
            clap_complete::Shell::Fish,
            &mut command,
            "papers",
            &mut buffer,
        ),
    }
    buffer
}

pub fn write(shell: Shell, out: &mut dyn Write) {
    let _ = out.write_all(&generate(shell));
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regenerate with: `cargo run -p cm-cli --bin papers -- completions <shell> > crates/cli/completions/<file>`.
    #[test]
    fn checked_in_completions_match_the_grammar() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("completions");
        for (shell, file) in [
            (Shell::Bash, "papers.bash"),
            (Shell::Zsh, "_papers"),
            (Shell::Fish, "papers.fish"),
        ] {
            let generated = String::from_utf8(generate(shell)).unwrap();
            assert!(generated.contains("papers"), "{file}");
            let checked_in = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            assert_eq!(checked_in, generated, "{file} is stale; regenerate it");
        }
    }
}
