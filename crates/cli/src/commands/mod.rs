//! Subcommand implementations. Each one requests the minimum capabilities (`credentials::Need`)
//! and talks to papersd through `connect::Daemon` only.
pub mod add;
pub mod completions;
pub mod export;
pub mod import;
pub mod mcp;
pub mod read;
pub mod server;

use crate::{
    Prompt,
    args::{Add, Cli, Command},
    connect::{Connection, Daemon, INTERACTIVE, Target},
    credentials::{CliCredential, Need, requested},
    output::{CliError, Kind, Report},
};
use cm_protocol::{
    domain::{Collection, CollectionId, Paper, PaperId, Tag, TagId, TagKey},
    import::ImportDestination,
    ipc::{Command as Ipc, Reply},
    organization::{Organization, OrganizationRequest},
};
use serde_json::{Value, json};
use std::io::Write;

pub enum Outcome {
    Report(Report),
    /// The command wrote its result to stdout itself (export without --output).
    Raw,
}

/// Envelope `command` name, e.g. "add doi", "server status".
pub fn name(command: &Command) -> String {
    match command {
        Command::Add(Add::Doi(_)) => "add doi".into(),
        Command::Add(Add::File(_)) => "add file".into(),
        Command::Import(_) => "import".into(),
        Command::Search(_) => "search".into(),
        Command::List(_) => "list".into(),
        Command::Show(_) => "show".into(),
        Command::Fulltext(_) => "fulltext".into(),
        Command::Annotations(_) => "annotations".into(),
        Command::Export(_) => "export".into(),
        Command::Server(s) => format!(
            "server {}",
            match s {
                crate::args::Server::Status => "status",
                crate::args::Server::Start { .. } => "start",
                crate::args::Server::Stop { .. } => "stop",
            }
        ),
        Command::Mcp => "mcp".into(),
        Command::Completions(_) => "completions".into(),
    }
}

pub fn dispatch(
    cli: &Cli,
    out: &mut dyn Write,
    err: &mut dyn Write,
    prompt: &mut dyn Prompt,
) -> Result<Outcome, CliError> {
    let report = match &cli.command {
        Command::Add(Add::Doi(args)) => {
            let mut daemon = open(cli, add::doi_need(args))?;
            add::doi(&mut daemon, args, prompt)?
        }
        Command::Add(Add::File(args)) => {
            let mut daemon = open(cli, add::file_need(args))?;
            add::file(&mut daemon, args, prompt)?
        }
        Command::Import(args) => {
            let need = if args.confirm.dry_run {
                Need::FilePreview
            } else {
                Need::FileWrite
            };
            let mut daemon = open(cli, need)?;
            import::run(&mut daemon, args, prompt)?
        }
        Command::Search(args) => read::search(&mut open(cli, Need::Read)?, args)?,
        Command::List(args) => read::list(&mut open(cli, Need::Read)?, args)?,
        Command::Show(args) => read::show(&mut open(cli, Need::Read)?, args)?,
        Command::Fulltext(args) => read::fulltext(&mut open(cli, Need::Read)?, args)?,
        Command::Annotations(args) => read::annotations(&mut open(cli, Need::Read)?, args)?,
        Command::Export(args) => {
            let target = target(cli)?;
            let mut daemon = Connection::open(&target, requested(Need::Export, cli.read_only))?;
            return export::run(&mut daemon, &target, args, out, err);
        }
        Command::Server(command) => server::run(&target(cli)?, cli.read_only, command)?,
        Command::Mcp | Command::Completions(_) => {
            return Err(CliError::internal("handled before dispatch"));
        }
    };
    Ok(Outcome::Report(report))
}

pub fn target(cli: &Cli) -> Result<Target, CliError> {
    let library = cli
        .library
        .as_deref()
        .ok_or_else(|| CliError::usage("no library: pass --library DIR or set PAPERS_LIBRARY"))?;
    Target::new(
        library,
        cli.runtime_dir.as_deref(),
        !cli.no_start,
        cli.daemon_idle_timeout_ms,
        CliCredential::from_option(cli.credential_file.as_deref()),
    )
}

fn open(cli: &Cli, need: Need) -> Result<Connection, CliError> {
    Connection::open(&target(cli)?, requested(need, cli.read_only))
}

/// Parses an internal UUID (paper/collection/tag/attachment ID).
pub fn parse_id<T: serde::de::DeserializeOwned>(raw: &str, what: &str) -> Result<T, CliError> {
    serde_json::from_value(Value::String(raw.trim().to_owned()))
        .map_err(|_| CliError::usage(format!("{what}: expected a UUID, got {raw:?}")))
}

pub fn paper_id(raw: &str) -> Result<PaperId, CliError> {
    parse_id(raw, "paper ID")
}

pub fn organization(daemon: &mut dyn Daemon) -> Result<Organization, CliError> {
    match daemon.call(Ipc::Organization(OrganizationRequest::List), INTERACTIVE)? {
        Reply::Organization(org) => Ok(org),
        _ => Err(unexpected()),
    }
}

pub fn unexpected() -> CliError {
    CliError::internal("daemon returned an unexpected reply")
}

/// Collection by ID or by (whitespace-collapsed) name; ambiguous names need the ID.
pub fn find_collection(org: &Organization, raw: &str) -> Result<CollectionId, CliError> {
    if let Ok(id) = parse_id::<CollectionId>(raw, "collection")
        && org.collections.iter().any(|c| c.id == id)
    {
        return Ok(id);
    }
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let found: Vec<&Collection> = org.collections.iter().filter(|c| c.name == name).collect();
    match found.as_slice() {
        [one] => Ok(one.id),
        [] => Err(CliError::new(
            Kind::NotFound,
            format!("no collection named {raw:?}"),
        )),
        many => Err(CliError::new(
            Kind::Conflict,
            format!(
                "collection name {raw:?} is ambiguous; use one of the IDs: {}",
                many.iter()
                    .map(|c| c.id.0.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// Tag by ID or by its normalized key (case-insensitive name).
pub fn find_tag(org: &Organization, raw: &str) -> Result<TagId, CliError> {
    if let Ok(id) = parse_id::<TagId>(raw, "tag")
        && org.tags.iter().any(|t| t.id == id)
    {
        return Ok(id);
    }
    let key = TagKey::new(raw).map_err(|e| CliError::usage(format!("tag {raw:?}: {e}")))?;
    org.tags
        .iter()
        .find(|t: &&Tag| t.key == key)
        .map(|t| t.id)
        .ok_or_else(|| CliError::new(Kind::NotFound, format!("no tag named {raw:?}")))
}

/// Existing collection/tag destination for new papers; nothing is created implicitly.
pub fn destination(
    daemon: &mut dyn Daemon,
    args: &crate::args::Destination,
) -> Result<ImportDestination, CliError> {
    if args.collection.is_none() && args.tag.is_empty() {
        return Ok(ImportDestination::default());
    }
    let org = organization(daemon)?;
    Ok(ImportDestination {
        collection: args
            .collection
            .as_deref()
            .map(|c| find_collection(&org, c))
            .transpose()?,
        tags: args
            .tag
            .iter()
            .map(|t| find_tag(&org, t))
            .collect::<Result<_, _>>()?,
    })
}

/// Compact paper projection used by list/search/import results.
pub fn paper_summary(paper: &Paper) -> Value {
    let b = &paper.bibliography;
    json!({
        "id": paper.id.0,
        "title": paper.title,
        "authors": b.authors.iter().filter(|a| a.role == "author").map(contributor).collect::<Vec<_>>(),
        "year": b.year,
        "journal": b.journal,
        "doi": b.doi,
        "citation_key": b.citation_key,
        "type": b.paper_type,
        "deleted": paper.deleted_at.is_some(),
    })
}

pub fn contributor(c: &cm_protocol::domain::Contributor) -> String {
    if let Some(literal) = &c.literal_name {
        return literal.clone();
    }
    match (&c.family_name, &c.given_name) {
        (Some(f), Some(g)) => format!("{f}, {g}"),
        (Some(f), None) => f.clone(),
        (None, Some(g)) => g.clone(),
        (None, None) => String::new(),
    }
}

/// Asks before a write unless `--yes`; non-interactive without `--yes` is an explicit refusal.
pub fn confirm(
    yes: bool,
    prompt: &mut dyn Prompt,
    question: &str,
    pending: impl FnOnce() -> Report,
) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    match prompt.confirm(question) {
        Some(true) => Ok(()),
        Some(false) => Err(
            CliError::new(Kind::ConfirmationRequired, "declined; nothing saved")
                .with_report(pending()),
        ),
        None => Err(CliError::new(
            Kind::ConfirmationRequired,
            "not saved: pass --yes to commit non-interactively (or --dry-run to preview)",
        )
        .with_report(pending())),
    }
}
