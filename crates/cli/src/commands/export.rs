//! `export`: papersd serializes BibTeX/RIS into a private scratch directory chosen by the CLI;
//! the CLI then streams it to stdout or publishes it atomically at `--output`.
use super::{Outcome, find_collection, organization, paper_id, unexpected};
use crate::{
    args::{BibFormat, ExportArgs},
    connect::{Daemon, LONG, Target},
    output::{CliError, Report, path_json, write_stdout},
    paths::{Scratch, export_target},
};
use cm_protocol::{
    exchange::{ExportRequest, ExportSelection, Format},
    ipc::{Command as Ipc, Reply},
};
use serde_json::json;
use std::io::Write;

/// The daemon caps output at 32 MiB; anything larger here is not its file.
const MAX_EXPORT_BYTES: u64 = 40 * 1024 * 1024;

pub fn run(
    daemon: &mut dyn Daemon,
    target: &Target,
    args: &ExportArgs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Outcome, CliError> {
    let selection = if !args.ids.is_empty() {
        ExportSelection::Ids(
            args.ids
                .iter()
                .map(|id| paper_id(id))
                .collect::<Result<_, _>>()?,
        )
    } else if let Some(collection) = &args.collection {
        ExportSelection::Collection(find_collection(&organization(daemon)?, collection)?)
    } else {
        ExportSelection::All
    };
    let (format, ext, label) = match args.format {
        BibFormat::Bibtex => (Format::Bibtex, "bib", "bibtex"),
        BibFormat::Ris => (Format::Ris, "ris", "ris"),
    };
    // Validate the destination before any work.
    let destination = args
        .output
        .as_deref()
        .map(|o| export_target(o, &target.paths.library, args.force))
        .transpose()?;
    let scratch_parent = match &destination {
        Some(d) => d.dir.clone(),
        None => std::env::temp_dir(),
    };
    let scratch = Scratch::new(&scratch_parent)?;
    let name = format!("export.{ext}");
    let exported = match daemon.call(
        Ipc::Export(ExportRequest {
            selection,
            format,
            path: scratch.dir.join(&name),
        }),
        LONG,
    )? {
        Reply::Exported(exported) => exported,
        _ => return Err(unexpected()),
    };
    match destination {
        None => {
            let bytes = scratch.read(&name, MAX_EXPORT_BYTES)?;
            // A closed pipe ends quietly with exit 0, like every other command.
            write_stdout(out, &bytes)?;
            let _ = writeln!(
                err,
                "papers: exported {} records ({label})",
                exported.records
            );
            for d in &exported.diagnostics {
                let _ = writeln!(err, "papers: warning: {d}");
            }
            let _ = writeln!(err, "papers: note: {}", exported.migration_notice);
            Ok(Outcome::Raw)
        }
        Some(destination) => {
            scratch.publish(&name, &destination)?;
            let mut report = Report::new(json!({
                "path": path_json(&destination.path),
                "format": label,
                "records": exported.records,
                "replaced": destination.replace,
                "diagnostics": exported.diagnostics,
                "migration_notice": exported.migration_notice,
            }))
            .line(format!(
                "exported {} records to {}",
                exported.records,
                destination.path.display()
            ));
            report.warnings.extend(exported.diagnostics.iter().cloned());
            Ok(Outcome::Report(report))
        }
    }
}
