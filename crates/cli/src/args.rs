//! Command-line grammar (clap derive). Behaviour lives in `commands`.
use crate::decide::OnDuplicate;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

const EXAMPLES: &str = "\
Examples:
  export PAPERS_LIBRARY=~/.local/share/citation-manager/library
  papers add doi 10.1234/example --yes
  papers add file ~/Downloads/article.pdf --dry-run
  papers add file ~/Downloads/article.pdf --attach-to <paper-id> --yes
  papers import library.bib --on-duplicate skip --yes
  papers search 'Li-Mg interface' --tag anode --json
  papers list --collection Reading --sort year
  papers show <paper-id>
  papers fulltext <paper-id> --page 5
  papers annotations <paper-id>
  papers export --format bibtex --collection Reading -o references.bib
  papers export --format ris --ids <id1>,<id2> -o selected.ris
  papers server status
  papers completions zsh > ~/.zfunc/_papers

stdout carries one JSON document (schema \"papers.v1\") or, with --text, human output;
diagnostics go to stderr. Exit codes: 0 ok, 1 other, 2 usage/invalid input, 3 not found,
4 conflict/duplicate/confirmation required, 5 permission denied, 6 daemon unavailable,
7 feature unavailable (MCP/REST). papersd is started on demand next to this executable.";

#[derive(Debug, Parser)]
#[command(
    name = "papers",
    version,
    about = "Citation manager CLI over the local papersd daemon (works without the GUI)",
    after_help = EXAMPLES,
    propagate_version = true
)]
pub struct Cli {
    /// Library directory (absolute or relative). Required for library commands.
    #[arg(long, global = true, env = "PAPERS_LIBRARY", value_name = "DIR")]
    pub library: Option<PathBuf>,
    /// Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager).
    #[arg(long, global = true, env = "PAPERS_RUNTIME_DIR", value_name = "DIR")]
    pub runtime_dir: Option<PathBuf>,
    /// Emit the JSON envelope on stdout (default).
    #[arg(long, global = true, overrides_with = "text")]
    pub json: bool,
    /// Emit human-readable text on stdout instead of JSON.
    #[arg(long, global = true, overrides_with = "json")]
    pub text: bool,
    /// Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5).
    #[arg(long, global = true)]
    pub read_only: bool,
    /// Present a credential issued by the daemon (0600 file) instead of the owner's desktop credential.
    #[arg(
        long,
        global = true,
        env = "PAPERS_CREDENTIAL_FILE",
        value_name = "FILE"
    )]
    pub credential_file: Option<PathBuf>,
    /// Never start papersd; fail with exit 6 when it is not running.
    #[arg(long, global = true)]
    pub no_start: bool,
    /// Idle timeout passed to a papersd that this invocation starts.
    #[arg(
        long,
        global = true,
        env = "PAPERS_DAEMON_IDLE_TIMEOUT_MS",
        value_name = "MS",
        value_parser = clap::value_parser!(u64).range(100..=86_400_000)
    )]
    pub daemon_idle_timeout_ms: Option<u64>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Register a paper by DOI or from a local PDF.
    #[command(subcommand)]
    Add(Add),
    /// Import a BibTeX or RIS file (preview, duplicate review, then commit).
    Import(ImportArgs),
    /// Full-text search over bibliography, annotations and extracted PDF text.
    Search(SearchArgs),
    /// List papers with filters, sort and keyset paging.
    List(ListArgs),
    /// Show one paper with its collections, tags and attachments.
    Show(ShowArgs),
    /// Read extracted PDF page text within page and character limits.
    Fulltext(FulltextArgs),
    /// List a paper's highlights, comments and notes.
    Annotations(AnnotationsArgs),
    /// Export BibTeX/RIS to a file or stdout.
    Export(ExportArgs),
    /// Inspect, start or stop the library's papersd.
    #[command(subcommand)]
    Server(Server),
    /// Serve MCP over stdio for this library (through papersd; never the desktop credential).
    Mcp,
    /// Print a shell completion script.
    Completions(CompletionsArgs),
}

#[derive(Debug, Args)]
pub struct Confirm {
    /// Commit without an interactive prompt (required when stdin is not a terminal).
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Preview only: report what would happen and save nothing.
    #[arg(long, conflicts_with = "yes")]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct Duplicates {
    /// What to do with a duplicate candidate. Merging always needs --merge.
    #[arg(long, value_enum, default_value_t = OnDuplicate::Ask)]
    pub on_duplicate: OnDuplicate,
}

#[derive(Debug, Args)]
pub struct Destination {
    /// Add the new paper to this existing collection (name or ID).
    #[arg(long, value_name = "NAME|ID")]
    pub collection: Option<String>,
    /// Add an existing tag (name or ID); repeatable.
    #[arg(long, value_name = "NAME|ID")]
    pub tag: Vec<String>,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// Merge into this reviewed duplicate candidate instead of creating a paper.
    #[arg(long, value_name = "PAPER_ID")]
    pub merge: Option<String>,
    /// Fields taken from the incoming record on --merge: none (default), all, or a list.
    #[arg(long, value_name = "none|all|FIELD,...", requires = "merge")]
    pub merge_fields: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Add {
    /// Look up a DOI (Crossref/DataCite), preview duplicates, then commit.
    Doi(AddDoiArgs),
    /// Register a local PDF as a new paper, or attach it to an existing paper.
    File(AddFileArgs),
}

#[derive(Debug, Args)]
pub struct AddDoiArgs {
    /// DOI or resolver URL.
    pub doi: String,
    /// Title to use when the lookup has none (or to override it).
    #[arg(long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub confirm: Confirm,
    #[command(flatten)]
    pub duplicates: Duplicates,
    #[command(flatten)]
    pub merge: MergeArgs,
    #[command(flatten)]
    pub destination: Destination,
}

#[derive(Debug, Args)]
pub struct AddFileArgs {
    /// PDF file to register or attach.
    pub path: PathBuf,
    /// Attach to this existing paper; its metadata is never changed by the file.
    #[arg(long, value_name = "PAPER_ID", conflicts_with_all = ["title", "doi", "merge", "collection", "tag"])]
    pub attach_to: Option<String>,
    /// Explicit title (otherwise PDF metadata, else a provisional title from the filename).
    #[arg(long)]
    pub title: Option<String>,
    /// Explicit DOI for the new paper.
    #[arg(long)]
    pub doi: Option<String>,
    #[command(flatten)]
    pub confirm: Confirm,
    #[command(flatten)]
    pub duplicates: Duplicates,
    #[command(flatten)]
    pub merge: MergeArgs,
    #[command(flatten)]
    pub destination: Destination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BibFormat {
    Bibtex,
    Ris,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// BibTeX (.bib) or RIS (.ris) file.
    pub path: PathBuf,
    /// Input format (default: from the file extension).
    #[arg(long, value_enum)]
    pub format: Option<BibFormat>,
    /// Rows per preview/commit batch.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub batch: u32,
    #[command(flatten)]
    pub confirm: Confirm,
    #[command(flatten)]
    pub duplicates: Duplicates,
    #[command(flatten)]
    pub destination: Destination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceKind {
    Bibliographic,
    Annotations,
    Pages,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Search DSL: terms, "phrases", doi:, tag:, year:, collection:, has:pdf.
    pub query: String,
    /// Require this tag (adds tag:"..." to the query); repeatable.
    #[arg(long)]
    pub tag: Vec<String>,
    /// Restrict to a collection (ID, or name added as collection:"...").
    #[arg(long, value_name = "NAME|ID")]
    pub collection: Option<String>,
    /// Sources to search (default: all).
    #[arg(long, value_enum, value_delimiter = ',')]
    pub sources: Vec<SourceKind>,
    /// Maximum hits (1-100).
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub limit: u32,
    /// Continue from a previous `next_cursor`/`continuation`.
    #[arg(long)]
    pub cursor: Option<String>,
    /// Snippet length in characters (32-1000).
    #[arg(long, default_value_t = 160, value_parser = clap::value_parser!(u32).range(32..=1000))]
    pub snippet_chars: u32,
    /// Scan PDF pages for 1-2 character terms without a narrowing filter (bounded windows).
    #[arg(long)]
    pub all_pages: bool,
    /// Include papers in the trash.
    #[arg(long)]
    pub include_trash: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortKey {
    Id,
    Title,
    Year,
    Created,
    Updated,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Direct members of this collection (name or ID).
    #[arg(long, value_name = "NAME|ID")]
    pub collection: Option<String>,
    /// Tag filter (name or ID); repeatable.
    #[arg(long)]
    pub tag: Vec<String>,
    /// Match any instead of all of the given tags.
    #[arg(long)]
    pub any_tag: bool,
    /// Literal substring filter over bibliographic fields.
    #[arg(long)]
    pub keyword: Option<String>,
    #[arg(long)]
    pub author: Option<String>,
    #[arg(long)]
    pub year_from: Option<i32>,
    #[arg(long)]
    pub year_to: Option<i32>,
    /// Only papers with (true) or without (false) a PDF attachment.
    #[arg(long)]
    pub has_pdf: Option<bool>,
    #[arg(long, value_enum, default_value_t = SortKey::Id)]
    pub sort: SortKey,
    #[arg(long)]
    pub desc: bool,
    /// Page size (1-500).
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=500))]
    pub limit: u32,
    /// Continue after a previous `next_cursor`.
    #[arg(long)]
    pub cursor: Option<String>,
    /// List papers in the trash instead of active ones.
    #[arg(long, conflicts_with = "include_trash")]
    pub trash: bool,
    /// Include trashed papers.
    #[arg(long)]
    pub include_trash: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    pub paper: String,
}

#[derive(Debug, Args)]
pub struct FulltextArgs {
    pub paper: String,
    /// One page by its 1-based physical number.
    #[arg(long, conflicts_with = "pages", value_parser = clap::value_parser!(u32).range(1..))]
    pub page: Option<u32>,
    /// Page range, 1-based inclusive (e.g. 3-7), at most 50 pages.
    #[arg(long, value_name = "FROM-TO")]
    pub pages: Option<String>,
    /// Pages to return when no --page/--pages is given (1-50).
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=50))]
    pub max_pages: u32,
    /// Total characters returned (1-400000).
    #[arg(long, default_value_t = 20_000, value_parser = clap::value_parser!(u32).range(1..=400_000))]
    pub max_chars: u32,
    /// Which PDF attachment (default: the paper's first).
    #[arg(long, value_name = "ATTACHMENT_ID")]
    pub attachment: Option<String>,
}

#[derive(Debug, Args)]
pub struct AnnotationsArgs {
    pub paper: String,
    /// Only this attachment's annotations.
    #[arg(long, value_name = "ATTACHMENT_ID")]
    pub attachment: Option<String>,
    /// Maximum annotations returned (1-1000).
    #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[arg(long, value_enum, default_value_t = BibFormat::Bibtex)]
    pub format: BibFormat,
    /// Comma-separated paper IDs.
    #[arg(long, value_delimiter = ',', conflicts_with = "collection")]
    pub ids: Vec<String>,
    /// Direct members of a collection (name or ID).
    #[arg(long, value_name = "NAME|ID")]
    pub collection: Option<String>,
    /// Destination file (outside the library). Without it the bibliography goes to stdout.
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Replace an existing regular file at --output.
    #[arg(long, requires = "output")]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum Server {
    /// Report whether papersd serves this library (never starts it).
    Status,
    /// Start papersd now (it exits by itself once idle).
    Start {
        /// Not a start option: REST follows the persisted API settings (`papersd api set`). Exits 7.
        #[arg(long)]
        rest: bool,
    },
    /// Stop papersd (SIGTERM). Refuses while other clients or jobs are active unless --force.
    Stop {
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    #[arg(value_enum)]
    pub shell: Shell,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn grammar_is_consistent() {
        Cli::command().debug_assert();
    }
}
