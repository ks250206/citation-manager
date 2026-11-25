//! `add doi` against a scripted daemon: the DOI resolution is mocked, so these tests cover the
//! lookup -> preview -> decision -> commit discipline without network or papersd.
use super::*;
use crate::{
    args::{Add, Cli, Command as CliCommand},
    connect::Daemon,
};
use clap::Parser;
use cm_protocol::import::{
    DuplicateCandidate, FieldDiff, ImportOutcome, MatchKind, PreviewCandidate, PreviewId,
};
use std::{collections::VecDeque, time::Duration};

struct Scripted {
    replies: VecDeque<Reply>,
    sent: Vec<Value>,
}
impl Daemon for Scripted {
    fn call(&mut self, command: Ipc, _timeout: Duration) -> Result<Reply, CliError> {
        self.sent.push(serde_json::to_value(&command).unwrap());
        Ok(self.replies.pop_front().unwrap_or(Reply::Unit))
    }
}
impl Scripted {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: replies.into(),
            sent: Vec::new(),
        }
    }
    /// Top-level command names, e.g. ["Resolve", "Import.Preview", "Import.Commit"].
    fn names(&self) -> Vec<String> {
        self.sent
            .iter()
            .map(|v| match v {
                Value::Object(o) => {
                    let (k, inner) = o.iter().next().unwrap();
                    match inner {
                        Value::Object(i) if k == "Import" => {
                            format!("{k}.{}", i.keys().next().unwrap())
                        }
                        _ => k.clone(),
                    }
                }
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect()
    }
}

struct Answer(Option<bool>);
impl crate::Prompt for Answer {
    fn confirm(&mut self, _question: &str) -> Option<bool> {
        self.0
    }
}

fn args(extra: &[&str]) -> AddDoiArgs {
    let mut argv = vec!["papers", "add", "doi", "10.1234/example"];
    argv.extend_from_slice(extra);
    match Cli::try_parse_from(argv).unwrap().command {
        CliCommand::Add(Add::Doi(args)) => args,
        _ => unreachable!(),
    }
}

fn resolution(title: &str) -> Reply {
    let r: MetadataResolution = serde_json::from_value(json!({
        "canonical_doi": "10.1234/example",
        "draft": {
            "title": title,
            "external_ids": [],
            "bibliography": {"doi": "10.1234/example", "year": 2024},
            "source": {"adapter": "crossref", "locator": null, "row": null, "sha256": null},
            "diagnostics": [],
        },
        "manual_add": title.is_empty(),
        "outcomes": [],
        "fields": [],
        "pdf_candidates": [],
    }))
    .unwrap();
    Reply::Resolution(Box::new(r))
}

fn existing(n: u128) -> cm_protocol::domain::Paper {
    serde_json::from_value(json!({
        "id": format!("00000000-0000-7000-8000-{n:012x}"),
        "title": "Existing paper",
        "external_ids": [],
        "doi": "10.1234/example",
    }))
    .unwrap()
}

fn preview(matches: &[MatchKind]) -> Reply {
    Reply::Import(ImportReply::Preview(ImportPreview {
        id: PreviewId("p1".into()),
        candidates: vec![PreviewCandidate {
            draft: PaperDraft::titled("Mocked DOI paper"),
            existing_match: None,
            matches: matches
                .iter()
                .enumerate()
                .map(|(i, &kind)| DuplicateCandidate {
                    existing: existing(i as u128 + 1),
                    kind,
                    differences: vec![FieldDiff {
                        field: "title".into(),
                        before: json!("Existing paper"),
                        after: json!("Mocked DOI paper"),
                    }],
                })
                .collect(),
            batch_matches: vec![],
            diagnostics: vec![],
        }],
        warnings: vec![],
    }))
}

fn committed() -> Reply {
    Reply::Import(ImportReply::Committed(ImportOutcome {
        created: vec![existing(9).id],
        ..Default::default()
    }))
}

#[test]
fn yes_commits_one_create_decision_after_preview() {
    let mut daemon = Scripted::new(vec![
        resolution("Mocked DOI paper"),
        preview(&[]),
        committed(),
    ]);
    let report = doi(&mut daemon, &args(&["--yes"]), &mut Answer(None)).unwrap();
    assert_eq!(
        daemon.names(),
        ["Resolve", "Import.Preview", "Import.Commit"]
    );
    assert_eq!(
        daemon.sent[2]["Import"]["Commit"]["decisions"][0]["action"],
        json!("Create")
    );
    assert_eq!(report.data["saved"], json!(true));
    assert_eq!(report.data["lookup"]["canonical_doi"], "10.1234/example");
    assert_eq!(report.data["result"]["created"][0], json!(existing(9).id.0));
}

#[test]
fn non_interactive_without_yes_saves_nothing_and_releases_the_preview() {
    let mut daemon = Scripted::new(vec![resolution("Mocked DOI paper"), preview(&[])]);
    let e = doi(&mut daemon, &args(&[]), &mut Answer(None)).unwrap_err();
    assert_eq!(e.kind, Kind::ConfirmationRequired);
    assert_eq!(
        daemon.names(),
        ["Resolve", "Import.Preview", "Import.Cancel"]
    );
    assert_eq!(e.report.unwrap().data["saved"], json!(false));

    // A terminal "y" is explicit permission.
    let mut daemon = Scripted::new(vec![
        resolution("Mocked DOI paper"),
        preview(&[]),
        committed(),
    ]);
    doi(&mut daemon, &args(&[]), &mut Answer(Some(true))).unwrap();
    assert_eq!(daemon.names().last().unwrap(), "Import.Commit");
}

#[test]
fn duplicates_stop_unless_skip_or_explicit_merge() {
    let mut daemon = Scripted::new(vec![
        resolution("Mocked DOI paper"),
        preview(&[MatchKind::Doi]),
    ]);
    let e = doi(&mut daemon, &args(&["--yes"]), &mut Answer(Some(true))).unwrap_err();
    assert_eq!(e.kind, Kind::Duplicate);
    assert!(!daemon.names().contains(&"Import.Commit".to_string()));
    let shown = &e.report.unwrap().data["preview"]["candidates"][0]["matches"][0];
    assert_eq!(shown["kind"], "doi");
    assert_eq!(shown["exact"], true);

    let mut daemon = Scripted::new(vec![
        resolution("Mocked DOI paper"),
        preview(&[MatchKind::Doi]),
    ]);
    let report = doi(
        &mut daemon,
        &args(&["--on-duplicate", "skip"]),
        &mut Answer(None),
    )
    .unwrap();
    assert_eq!(report.data["saved"], json!(false));
    assert_eq!(
        daemon.names(),
        ["Resolve", "Import.Preview", "Import.Cancel"]
    );

    let target = existing(1).id.0.to_string();
    let mut daemon = Scripted::new(vec![
        resolution("Mocked DOI paper"),
        preview(&[MatchKind::Doi]),
        Reply::Import(ImportReply::Committed(ImportOutcome {
            merged: vec![existing(1).id],
            ..Default::default()
        })),
    ]);
    doi(
        &mut daemon,
        &args(&["--merge", &target, "--merge-fields", "title", "--yes"]),
        &mut Answer(None),
    )
    .unwrap();
    assert_eq!(
        daemon.sent[2]["Import"]["Commit"]["decisions"][0]["action"],
        json!({"Merge": {"match_index": 0, "fields": ["title"]}})
    );
}

#[test]
fn dry_run_and_unresolved_metadata_never_commit() {
    let mut daemon = Scripted::new(vec![resolution("Mocked DOI paper"), preview(&[])]);
    let report = doi(&mut daemon, &args(&["--dry-run"]), &mut Answer(Some(true))).unwrap();
    assert_eq!(report.data["dry_run"], json!(true));
    assert_eq!(
        daemon.names(),
        ["Resolve", "Import.Preview", "Import.Cancel"]
    );

    let mut daemon = Scripted::new(vec![resolution("")]);
    let e = doi(&mut daemon, &args(&["--yes"]), &mut Answer(None)).unwrap_err();
    assert_eq!(e.kind, Kind::Unresolved);
    assert_eq!(daemon.names(), ["Resolve"]);
    assert_eq!(e.report.unwrap().data["unresolved"], json!(["title"]));

    // An explicit title completes a manual registration through the same preview.
    let mut daemon = Scripted::new(vec![resolution(""), preview(&[]), committed()]);
    doi(
        &mut daemon,
        &args(&["--title", "Manual", "--yes"]),
        &mut Answer(None),
    )
    .unwrap();
    assert_eq!(
        daemon.sent[1]["Import"]["Preview"]["candidates"][0]["title"],
        json!("Manual")
    );
}

#[test]
fn add_file_dry_run_never_requests_write() {
    use cm_protocol::ipc::Capability;
    let parse = |extra: &[&str]| {
        let mut argv = vec!["papers", "add", "file", "x.pdf"];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv).unwrap().command {
            CliCommand::Add(Add::File(args)) => file_need(&args),
            _ => unreachable!(),
        }
    };
    let id = "00000000-0000-7000-8000-000000000001";
    for need in [
        parse(&["--dry-run"]),
        parse(&["--dry-run", "--attach-to", id]),
    ] {
        assert!(
            !need.capabilities().contains(&Capability::LibraryWrite),
            "{need:?}"
        );
    }
    assert!(
        parse(&["--yes"])
            .capabilities()
            .contains(&Capability::LibraryWrite)
    );
}
