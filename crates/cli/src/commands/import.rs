//! `import`: parse a BibTeX/RIS file in papersd, review every batch BEFORE committing any of
//! them, then commit batch by batch with the same explicit policy. Merging is not offered for
//! bulk import (use `add` with `--merge <id>` or the GUI).
use super::{
    add::{cancel_preview, import_preview, outcome_json},
    confirm, destination, unexpected,
};
use crate::{
    Prompt,
    args::{BibFormat, ImportArgs},
    connect::{Daemon, LONG},
    decide::{self, Decision, MergeFields, Policy},
    output::{CliError, Kind, Report, path_json},
    paths,
};
use cm_protocol::{
    exchange::{Format, ImportReply, ImportRequestUi, ParsedFile},
    import::{ImportAction, ImportCommit, ImportDecision, ImportDestination},
    ipc::{Command as Ipc, Reply},
};
use serde_json::{Value, json};

pub fn run(
    daemon: &mut dyn Daemon,
    args: &ImportArgs,
    prompt: &mut dyn Prompt,
) -> Result<Report, CliError> {
    let path = paths::input_file(&args.path)?;
    let format = match args.format {
        Some(BibFormat::Bibtex) => Format::Bibtex,
        Some(BibFormat::Ris) => Format::Ris,
        None => match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("bib" | "bibtex") => Format::Bibtex,
            Some("ris") => Format::Ris,
            _ => {
                return Err(CliError::usage(
                    "cannot infer the format; pass --format bibtex|ris",
                ));
            }
        },
    };
    let policy = Policy {
        on_duplicate: args.duplicates.on_duplicate,
        merge: None,
        merge_fields: MergeFields::None,
        skip_invalid: true,
    };
    let destination = destination(daemon, &args.destination)?;
    let parsed = match daemon.call(
        Ipc::Import(ImportRequestUi::Parse {
            path: path.clone(),
            format,
        }),
        LONG,
    )? {
        Reply::Import(ImportReply::Parsed(parsed)) => parsed,
        _ => return Err(unexpected()),
    };
    let batch = args.batch as usize;
    let mut report = Report::new(json!({
        "path": path_json(&path),
        "format": match format { Format::Bibtex => "bibtex", Format::Ris => "ris" },
        "records": parsed.records,
        "file_diagnostics": file_diagnostics(&parsed),
        "migration_notice": parsed.migration_notice,
        "dry_run": args.confirm.dry_run,
    }));
    report.warnings.extend(
        parsed
            .diagnostics
            .iter()
            .map(|d| format!("line {}: {}", d.line, d.message)),
    );
    report
        .text
        .push(format!("{}: {} records", path.display(), parsed.records));

    // Pass 1: review every batch; nothing is committed if any row still needs a decision.
    let (rows, plan) = review(daemon, &parsed, batch, &destination, &policy)?;
    let summary = summarize(&plan);
    report.data["plan"] = summary.clone();
    report.data["rows"] = json!(rows);
    report.text.push(format!("plan: {summary}"));
    if let Some(invalid) = summary.get("skip_invalid") {
        report.warnings.push(format!(
            "{invalid} invalid rows will be skipped (see rows[].diagnostics)"
        ));
    }
    let undecided = plan
        .iter()
        .filter(|d| matches!(d, Decision::Undecided { .. }))
        .count();
    if args.confirm.dry_run {
        report.data["saved"] = json!(false);
        if undecided > 0 {
            report.warnings.push(format!(
                "{undecided} rows need a decision (--on-duplicate skip|create)"
            ));
        }
        return Ok(report.line("dry run: nothing saved"));
    }
    if let Some(mut e) = decide::first_undecided(&plan) {
        report.data["saved"] = json!(false);
        e.message = format!(
            "{undecided} rows need a decision (first: {}); nothing saved",
            e.message
        );
        return Err(e.with_report(report));
    }
    let writes = plan
        .iter()
        .filter(|d| matches!(d, Decision::Act { action, .. } if *action != ImportAction::Skip))
        .count();
    if writes == 0 {
        report.data["saved"] = json!(false);
        return Ok(report.line("nothing to import"));
    }
    report.data["saved"] = json!(false);
    confirm(
        args.confirm.yes,
        prompt,
        &format!("Import {writes} records?"),
        || report.clone(),
    )?;

    // Pass 2: commit each batch after a fresh preview with the same policy.
    let mut created = Vec::new();
    let mut skipped = 0usize;
    let mut committed_rows = Vec::new();
    for start in (0..parsed.records).step_by(batch) {
        let count = batch.min(parsed.records - start);
        let preview = import_preview(
            daemon,
            ImportRequestUi::PreviewFile {
                start,
                count,
                destination: destination.clone(),
            },
        )?;
        let decisions: Vec<Decision> = preview
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| decide::decide(c, i, &policy))
            .collect();
        if let Some(e) = decide::first_undecided(&decisions) {
            cancel_preview(daemon, &preview);
            report.data["saved"] = json!(!created.is_empty());
            report.data["result"] = json!({ "created": created, "skipped": skipped, "rows": committed_rows, "stopped_at": start });
            return Err(CliError::new(
                Kind::Conflict,
                format!(
                    "library changed during import (row {}): {}; earlier batches remain saved",
                    start, e.message
                ),
            )
            .with_report(report));
        }
        let commit = ImportCommit {
            preview_id: preview.id.clone(),
            decisions: decisions
                .into_iter()
                .enumerate()
                .map(|(i, d)| ImportDecision {
                    candidate_index: i,
                    action: match d {
                        Decision::Act { action, .. } => action,
                        Decision::Undecided { .. } => ImportAction::Skip,
                    },
                })
                .collect(),
        };
        let outcome = match daemon.call(Ipc::Import(ImportRequestUi::Commit(commit)), LONG) {
            Ok(Reply::Import(ImportReply::Committed(outcome))) => outcome,
            Ok(_) => return Err(unexpected()),
            Err(mut e) => {
                report.data["saved"] = json!(!created.is_empty());
                report.data["result"] = json!({ "created": created, "skipped": skipped, "rows": committed_rows, "stopped_at": start });
                e.message = format!(
                    "{} (batch at row {start}); earlier batches remain saved",
                    e.message
                );
                return Err(e.with_report(report));
            }
        };
        let (value, _) = outcome_json(&outcome);
        created.extend(outcome.created.iter().map(|p| p.0));
        skipped += outcome.skipped;
        for mut row in value["rows"].as_array().cloned().unwrap_or_default() {
            row["index"] = json!(start + row["index"].as_u64().unwrap_or(0) as usize);
            committed_rows.push(row);
        }
    }
    report.data["saved"] = json!(true);
    report
        .text
        .push(format!("created {}, skipped {skipped}", created.len()));
    report.data["result"] =
        json!({ "created": created, "skipped": skipped, "rows": committed_rows });
    Ok(report)
}

fn review(
    daemon: &mut dyn Daemon,
    parsed: &ParsedFile,
    batch: usize,
    destination: &ImportDestination,
    policy: &Policy,
) -> Result<(Vec<Value>, Vec<Decision>), CliError> {
    let mut rows = Vec::new();
    let mut plan = Vec::new();
    // Rows of earlier batches are not persisted during review, so the daemon cannot match
    // them: carry their exact identifiers forward so every decision is known before commit.
    let mut seen = decide::SeenIdentifiers::default();
    for start in (0..parsed.records).step_by(batch) {
        let count = batch.min(parsed.records - start);
        let preview = import_preview(
            daemon,
            ImportRequestUi::PreviewFile {
                start,
                count,
                destination: destination.clone(),
            },
        )?;
        for (i, candidate) in preview.candidates.iter().enumerate() {
            let decision = seen.check(
                candidate,
                start + i,
                decide::decide(candidate, i, policy),
                policy,
            );
            rows.push(decide::candidate_json(candidate, start + i, &decision));
            plan.push(decision);
        }
        cancel_preview(daemon, &preview);
    }
    Ok((rows, plan))
}

fn summarize(plan: &[Decision]) -> Value {
    let mut counts = serde_json::Map::new();
    for decision in plan {
        let key = match decision {
            Decision::Act { label, .. } => *label,
            Decision::Undecided { .. } => "needs_decision",
        };
        let entry = counts.entry(key).or_insert(json!(0));
        *entry = json!(entry.as_u64().unwrap_or(0) + 1);
    }
    Value::Object(counts)
}

fn file_diagnostics(parsed: &ParsedFile) -> Value {
    json!(
        parsed
            .diagnostics
            .iter()
            .map(|d| json!({ "line": d.line, "message": d.message, "error": d.error }))
            .collect::<Vec<_>>()
    )
}
