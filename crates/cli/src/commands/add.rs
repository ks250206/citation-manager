//! `add doi` and `add file`: lookup/inspect -> common #33 preview -> explicit decision ->
//! commit through papersd. Duplicates never merge without `--merge <id>`.
use super::{confirm, destination, paper_id, paper_summary, unexpected};
use crate::{
    Prompt,
    args::{AddDoiArgs, AddFileArgs, Confirm, Duplicates, MergeArgs},
    connect::{Daemon, INTERACTIVE, LONG},
    credentials::Need,
    decide::{self, Decision, MergeFields, Policy},
    output::{CliError, Kind, Report},
    paths,
};
use cm_protocol::{
    attachments::{FileDisposition, PdfFile, PdfIntent, PdfReply, Selection},
    domain::PaperId,
    exchange::{ImportReply, ImportRequestUi, MetadataResolution},
    import::{
        ImportAction, ImportCommit, ImportDecision, ImportOutcome, ImportPreview, ImportRequest,
        PaperDraft,
    },
    ipc::{Command as Ipc, PdfCommand, Reply},
    library::{LibraryReply, LibraryRequest},
};
use serde_json::{Value, json};

pub fn doi_need(args: &AddDoiArgs) -> Need {
    if args.confirm.dry_run {
        Need::LookupPreview
    } else {
        Need::LookupWrite
    }
}

pub fn file_need(args: &AddFileArgs) -> Need {
    match (&args.attach_to, args.confirm.dry_run) {
        (Some(_), true) => Need::FileInspect,
        _ => Need::FileWrite,
    }
}

pub fn policy(duplicates: &Duplicates, merge: &MergeArgs) -> Result<Policy, CliError> {
    Ok(Policy {
        on_duplicate: duplicates.on_duplicate,
        merge: merge.merge.as_deref().map(paper_id).transpose()?,
        merge_fields: MergeFields::parse(merge.merge_fields.as_deref()),
        skip_invalid: false,
    })
}

pub fn doi(
    daemon: &mut dyn Daemon,
    args: &AddDoiArgs,
    prompt: &mut dyn Prompt,
) -> Result<Report, CliError> {
    let policy = policy(&args.duplicates, &args.merge)?;
    let destination = destination(daemon, &args.destination)?;
    let resolution = match daemon.call(
        Ipc::Resolve {
            doi: args.doi.clone(),
        },
        INTERACTIVE,
    )? {
        Reply::Resolution(r) => *r,
        _ => return Err(unexpected()),
    };
    let lookup = resolution_json(&resolution);
    let mut draft = resolution.draft.clone();
    if let Some(title) = &args.title {
        draft.title = title.clone();
    }
    if draft.title.trim().is_empty() {
        let report = Report::new(json!({
            "lookup": lookup,
            "unresolved": ["title"],
            "saved": false,
        }))
        .line(format!(
            "DOI {}: metadata unavailable or incomplete",
            resolution.canonical_doi
        ));
        let mut report = report;
        for o in &resolution.outcomes {
            report
                .text
                .push(format!("  {}: {:?}", o.provider.name(), o.status));
        }
        return Err(CliError::new(
            Kind::Unresolved,
            "DOI metadata unavailable or has no title; nothing saved. Pass --title to register it manually",
        )
        .with_report(report));
    }
    let preview = import_preview(
        daemon,
        ImportRequestUi::Preview(ImportRequest {
            candidates: vec![draft],
            destination,
        }),
    )?;
    let mut text = vec![format!("DOI {}", resolution.canonical_doi)];
    for o in &resolution.outcomes {
        text.push(format!("  {}: {:?}", o.provider.name(), o.status));
    }
    review_and_commit(
        daemon,
        preview,
        &policy,
        &args.confirm,
        prompt,
        Report {
            data: json!({ "lookup": lookup }),
            text,
            warnings: Vec::new(),
        },
        |daemon, commit| {
            let outcome = match daemon.call(Ipc::Import(ImportRequestUi::Commit(commit)), LONG)? {
                Reply::Import(ImportReply::Committed(outcome)) => outcome,
                _ => return Err(unexpected()),
            };
            Ok(outcome_json(&outcome))
        },
    )
}

pub fn file(
    daemon: &mut dyn Daemon,
    args: &AddFileArgs,
    prompt: &mut dyn Prompt,
) -> Result<Report, CliError> {
    let path = paths::input_file(&args.path)?;
    let attach = args.attach_to.as_deref().map(paper_id).transpose()?;
    let policy = policy(&args.duplicates, &args.merge)?;
    let destination = if attach.is_some() {
        Default::default()
    } else {
        destination(daemon, &args.destination)?
    };
    // Check the target paper first so a typo fails before the file is hashed.
    let target = attach.map(|id| get_paper(daemon, id)).transpose()?;
    let intent = attach.map_or(PdfIntent::RegisterNew, PdfIntent::AttachExisting);
    let file = inspect(daemon, path, intent)?;
    let mut data = json!({ "file": file_json(&file) });
    if file.disposition != FileDisposition::Ready {
        let (kind, message) = refused(&file.disposition);
        return Err(
            CliError::new(kind, format!("{}: {message}; nothing saved", file.name))
                .with_report(Report::new(data)),
        );
    }
    if let (Some(paper), Some(summary)) = (attach, target) {
        data["mode"] = json!("attach");
        data["paper"] = summary;
        let mut report =
            Report::new(data).line(format!("attach {} to paper {}", file.name, paper.0));
        if args.confirm.dry_run {
            report.data["dry_run"] = json!(true);
            report.data["saved"] = json!(false);
            return Ok(report);
        }
        confirm(
            args.confirm.yes,
            prompt,
            &format!("Attach {} to {}?", file.name, paper.0),
            || report.clone(),
        )?;
        let after = pdf_report(daemon.call(
            Ipc::Pdf(PdfCommand::Attach {
                indices: vec![0],
                paper,
            }),
            LONG,
        )?)?;
        report.data["file"] = file_json(&after);
        report.data["dry_run"] = json!(false);
        return match after.attachment {
            Some(attachment) => {
                report.data["saved"] = json!(true);
                report.data["attachment_id"] = json!(attachment.0);
                Ok(report.line(format!("attached {}", attachment.0)))
            }
            None => {
                report.data["saved"] = json!(false);
                Err(CliError::internal(format!(
                    "attachment failed: {}",
                    refused(&after.disposition).1
                ))
                .with_report(report))
            }
        };
    }

    // New paper: explicit input first, then PDF metadata, else a provisional filename title.
    let explicit = (args.title.is_some() || args.doi.is_some()).then(|| {
        let mut draft = PaperDraft::titled(args.title.clone().unwrap_or_default());
        draft.bibliography.doi = args.doi.clone();
        Box::new(draft)
    });
    let unresolved: Vec<&String> = file
        .messages
        .iter()
        .filter(|m| args.title.is_none() || !m.contains("provisional title"))
        .collect();
    data["mode"] = json!("new");
    data["provisional_title"] = json!(
        args.title.is_none()
            && file
                .messages
                .iter()
                .any(|m| m.contains("provisional title"))
    );
    data["unresolved"] = json!(unresolved);
    let mut text = vec![format!("register {} as a new paper", file.name)];
    text.extend(unresolved.iter().map(|m| format!("  unresolved: {m}")));
    let preview = match daemon.call(
        Ipc::Pdf(PdfCommand::PreviewNew {
            index: 0,
            explicit,
            destination,
        }),
        LONG,
    )? {
        Reply::Pdf(PdfReply::Preview(preview)) => preview,
        _ => return Err(unexpected()),
    };
    let mut report = Report {
        data,
        text,
        warnings: Vec::new(),
    };
    report.warnings.extend(
        unresolved
            .iter()
            .map(|m| format!("unresolved metadata: {m}")),
    );
    let path_hint = args.path.display().to_string();
    review_and_commit(
        daemon,
        preview,
        &policy,
        &args.confirm,
        prompt,
        report,
        |daemon, commit| {
            let after = pdf_report(
                daemon.call(Ipc::Pdf(PdfCommand::CommitNew { index: 0, commit }), LONG)?,
            )?;
            let mut result = json!({
                "file": file_json(&after),
                "paper_id": after.paper.map(|p| p.0),
                "attachment_id": after.attachment.map(|a| a.0),
            });
            let mut lines = Vec::new();
            match (after.paper, after.attachment) {
                (Some(paper), Some(attachment)) => {
                    lines.push(format!(
                        "created paper {} with attachment {}",
                        paper.0, attachment.0
                    ));
                }
                (Some(paper), None) => {
                    result["saved"] = json!("paper_only");
                    return Err(CliError::internal(format!(
                    "paper {} saved but attaching the PDF failed: {}. Retry: papers add file {} --attach-to {} --yes",
                    paper.0,
                    refused(&after.disposition).1,
                    path_hint,
                    paper.0
                ))
                .with_report(Report::new(json!({ "result": result }))));
                }
                (None, _) => lines.push("skipped; nothing saved".into()),
            }
            Ok((result, lines))
        },
    )
}

fn get_paper(daemon: &mut dyn Daemon, id: PaperId) -> Result<Value, CliError> {
    match daemon.call(Ipc::Library(LibraryRequest::Get(id)), INTERACTIVE)? {
        Reply::Library(LibraryReply::Paper(paper)) => Ok(paper_summary(&paper)),
        _ => Err(unexpected()),
    }
}

fn inspect(
    daemon: &mut dyn Daemon,
    path: std::path::PathBuf,
    intent: PdfIntent,
) -> Result<PdfFile, CliError> {
    pdf_report(daemon.call(
        Ipc::Pdf(PdfCommand::Inspect {
            selection: Selection::Files(vec![path]),
            intent,
        }),
        LONG,
    )?)
}

fn pdf_report(reply: Reply) -> Result<PdfFile, CliError> {
    match reply {
        Reply::Pdf(PdfReply::Report(report)) => {
            report.files.into_iter().next().ok_or_else(unexpected)
        }
        _ => Err(unexpected()),
    }
}

fn refused(disposition: &FileDisposition) -> (Kind, String) {
    match disposition {
        FileDisposition::Ready => (Kind::Internal, "ready".into()),
        FileDisposition::DuplicateHash => {
            (Kind::Conflict, "duplicate file in this selection".into())
        }
        FileDisposition::NonPdf => (Kind::InvalidInput, "not a PDF file".into()),
        FileDisposition::Oversized => (Kind::InvalidInput, "file exceeds the 256 MiB limit".into()),
        FileDisposition::UnsafeLink => (
            Kind::InvalidInput,
            "symlinks and unsafe paths are refused".into(),
        ),
        FileDisposition::OutsideScope => (Kind::InvalidInput, "outside the selected scope".into()),
        FileDisposition::Failed(reason) => (Kind::InvalidInput, reason.clone()),
        FileDisposition::Cancelled => (Kind::Internal, "cancelled".into()),
        FileDisposition::Imported(_) => (Kind::Conflict, "already imported".into()),
    }
}

fn disposition_name(d: &FileDisposition) -> &'static str {
    match d {
        FileDisposition::Ready => "ready",
        FileDisposition::OutsideScope => "outside_scope",
        FileDisposition::UnsafeLink => "unsafe_link",
        FileDisposition::NonPdf => "non_pdf",
        FileDisposition::Oversized => "oversized",
        FileDisposition::DuplicateHash => "duplicate_hash",
        FileDisposition::Failed(_) => "failed",
        FileDisposition::Cancelled => "cancelled",
        FileDisposition::Imported(_) => "imported",
    }
}

fn file_json(file: &PdfFile) -> Value {
    json!({
        "path": file.path,
        "name": file.name,
        "bytes": file.bytes,
        "sha256": file.sha256.map(hex),
        "disposition": disposition_name(&file.disposition),
        "detail": match &file.disposition { FileDisposition::Failed(r) => Some(r.clone()), _ => None },
        "messages": file.messages,
        "draft": file.draft.as_ref().map(decide::draft_json),
        "paper_id": file.paper.map(|p| p.0),
        "attachment_id": file.attachment.map(|a| a.0),
    })
}

pub fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn resolution_json(r: &MetadataResolution) -> Value {
    json!({
        "canonical_doi": r.canonical_doi,
        "manual_add": r.manual_add,
        "providers": r.outcomes.iter().map(|o| json!({
            "provider": o.provider.name(),
            "status": format!("{:?}", o.status),
            "attempts": o.attempts,
            "cached": o.cached,
        })).collect::<Vec<_>>(),
        "fields": r.fields.iter().map(|f| json!({
            "field": f.field, "provider": f.provider.name(), "selected": f.selected, "value": f.value,
        })).collect::<Vec<_>>(),
        // Suggestions only: never fetched, no access/license proof.
        "pdf_candidates": r.pdf_candidates.iter().map(|p| &p.url).collect::<Vec<_>>(),
    })
}

pub fn outcome_json(outcome: &ImportOutcome) -> (Value, Vec<String>) {
    let mut lines = Vec::new();
    lines.extend(outcome.created.iter().map(|id| format!("created {}", id.0)));
    lines.extend(
        outcome
            .merged
            .iter()
            .map(|id| format!("merged into {}", id.0)),
    );
    if outcome.skipped > 0 {
        lines.push(format!("skipped {}", outcome.skipped));
    }
    (
        json!({
            "created": outcome.created.iter().map(|p| p.0).collect::<Vec<_>>(),
            "merged": outcome.merged.iter().map(|p| p.0).collect::<Vec<_>>(),
            "skipped": outcome.skipped,
            "rows": outcome.rows.iter().map(|r| json!({
                "index": r.candidate_index,
                "paper_id": r.paper.map(|p| p.0),
                "source_row": r.source.row,
                "diagnostics": r.diagnostics.iter().map(|d| json!({
                    "field": d.field, "message": d.message, "error": d.error,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
        lines,
    )
}

pub fn import_preview(
    daemon: &mut dyn Daemon,
    request: ImportRequestUi,
) -> Result<ImportPreview, CliError> {
    match daemon.call(Ipc::Import(request), LONG)? {
        Reply::Import(ImportReply::Preview(preview)) => Ok(preview),
        _ => Err(unexpected()),
    }
}

/// Best effort: releases the daemon's retained preview state.
pub fn cancel_preview(daemon: &mut dyn Daemon, preview: &ImportPreview) {
    let _ = daemon.call(
        Ipc::Import(ImportRequestUi::Cancel(preview.id.clone())),
        INTERACTIVE,
    );
}

/// Shows the single-row preview, applies the explicit policy and commits only when allowed.
fn review_and_commit(
    daemon: &mut dyn Daemon,
    preview: ImportPreview,
    policy: &Policy,
    confirmation: &Confirm,
    prompt: &mut dyn Prompt,
    mut report: Report,
    commit: impl FnOnce(&mut dyn Daemon, ImportCommit) -> Result<(Value, Vec<String>), CliError>,
) -> Result<Report, CliError> {
    let candidate = preview.candidates.first().ok_or_else(unexpected)?;
    let decision = decide::decide(candidate, 0, policy);
    report.data["preview"] = json!({
        "candidates": [decide::candidate_json(candidate, 0, &decision)],
        "warnings": preview.warnings,
    });
    report
        .text
        .push(format!("title: {}", candidate.draft.title));
    for m in &candidate.matches {
        report.text.push(format!(
            "  {} duplicate: {} {}",
            decide::match_kind(m.kind),
            m.existing.id.0,
            m.existing.title
        ));
    }
    report.text.push(format!(
        "decision: {}",
        decide::decision_json(&decision)["action"]
            .as_str()
            .unwrap_or("?")
    ));
    report.data["dry_run"] = json!(confirmation.dry_run);
    report.data["saved"] = json!(false);
    if confirmation.dry_run {
        cancel_preview(daemon, &preview);
        return Ok(report.line("dry run: nothing saved"));
    }
    let action = match decision {
        Decision::Undecided { kind, reason } => {
            cancel_preview(daemon, &preview);
            return Err(CliError::new(kind, format!("{reason}; nothing saved")).with_report(report));
        }
        Decision::Act {
            action: ImportAction::Skip,
            ..
        } => {
            cancel_preview(daemon, &preview);
            return Ok(report.line("skipped; nothing saved"));
        }
        Decision::Act { action, .. } => action,
    };
    let question = match &action {
        ImportAction::Merge { .. } => "Merge into the selected existing paper?",
        _ => "Save this paper?",
    };
    if let Err(e) = confirm(confirmation.yes, prompt, question, || report.clone()) {
        cancel_preview(daemon, &preview);
        return Err(e);
    }
    let committed = commit(
        daemon,
        ImportCommit {
            preview_id: preview.id.clone(),
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action,
            }],
        },
    );
    let (result, lines) = match committed {
        Ok(done) => done,
        Err(mut e) => {
            // Keep the preview context and any partial result (e.g. paper saved, PDF not).
            if let Some(partial) = e.report.take() {
                report.data["result"] = partial.data["result"].clone();
            }
            e.report = Some(report);
            return Err(e);
        }
    };
    report.data["saved"] = json!(true);
    report.data["result"] = result;
    report.text.extend(lines);
    Ok(report)
}

#[cfg(test)]
mod tests;
