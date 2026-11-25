//! Non-interactive decisions over a common #33 import preview. Nothing is merged or created
//! over a duplicate unless the user said so with `--merge <id>` or `--on-duplicate`.
use crate::output::{CliError, Kind};
use cm_protocol::{
    domain::PaperId,
    import::{ImportAction, MatchKind, PreviewCandidate},
};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnDuplicate {
    /// Stop with exit 4 and show the candidates (default).
    #[default]
    Ask,
    /// Skip rows that match an existing paper (exactly or approximately).
    Skip,
    /// Create a separate paper when the match is only approximate. Exact identifier matches
    /// (DOI/arXiv/PMID) can never be created twice; they still need skip or `--merge`.
    Create,
}

/// Which differing fields an explicit merge takes from the incoming record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MergeFields {
    /// Keep the existing metadata; only add destination membership.
    #[default]
    None,
    All,
    Only(Vec<String>),
}
impl MergeFields {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") | Some("none") => Self::None,
            Some("all") => Self::All,
            Some(list) => Self::Only(
                list.split(',')
                    .map(|f| f.trim().to_owned())
                    .filter(|f| !f.is_empty())
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub on_duplicate: OnDuplicate,
    pub merge: Option<PaperId>,
    pub merge_fields: MergeFields,
    /// Rows the shared normalizer marked invalid are skipped (bulk import) instead of failing.
    pub skip_invalid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Act {
        action: ImportAction,
        /// Machine label: create, merge, skip_duplicate, skip_invalid.
        label: &'static str,
    },
    Undecided {
        kind: Kind,
        reason: String,
    },
}

/// Exact persisted matches plus exact matches with an EARLIER row of the same batch that may
/// still be created (`counts`); later rows are duplicates of that first occurrence.
fn exact(
    candidate: &PreviewCandidate,
    index: usize,
    counts: &dyn Fn(usize) -> bool,
) -> (Vec<PaperId>, Vec<usize>) {
    (
        candidate
            .matches
            .iter()
            .filter(|m| m.kind.exact())
            .map(|m| m.existing.id)
            .collect(),
        candidate
            .batch_matches
            .iter()
            .copied()
            .filter(|&j| j < index && counts(j))
            .collect(),
    )
}

/// Decides every row of one preview batch in order. An earlier row that is skipped (invalid or
/// itself a duplicate) is never saved, so it does not make a later row with the same
/// identifier a duplicate.
pub fn decide_batch(candidates: &[PreviewCandidate], policy: &Policy) -> Vec<Decision> {
    let mut decisions: Vec<Decision> = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let skipped = |j: usize| {
            matches!(
                decisions.get(j),
                Some(Decision::Act {
                    action: ImportAction::Skip,
                    ..
                })
            )
        };
        let decision = decide_with(candidate, index, policy, &|j| !skipped(j));
        decisions.push(decision);
    }
    decisions
}

/// Decides one row on its own, treating every earlier batch match as a duplicate.
pub fn decide(candidate: &PreviewCandidate, index: usize, policy: &Policy) -> Decision {
    decide_with(candidate, index, policy, &|_| true)
}

fn decide_with(
    candidate: &PreviewCandidate,
    index: usize,
    policy: &Policy,
    counts: &dyn Fn(usize) -> bool,
) -> Decision {
    let undecided = |kind, reason: String| Decision::Undecided { kind, reason };
    if candidate.diagnostics.iter().any(|d| d.error) {
        let reason = candidate
            .diagnostics
            .iter()
            .filter(|d| d.error)
            .map(|d| format!("{}: {}", d.field, d.message))
            .collect::<Vec<_>>()
            .join("; ");
        return if policy.skip_invalid {
            Decision::Act {
                action: ImportAction::Skip,
                label: "skip_invalid",
            }
        } else {
            undecided(
                Kind::Unresolved,
                format!("metadata is invalid and can only be skipped: {reason}"),
            )
        };
    }
    if let Some(target) = policy.merge {
        let Some(match_index) = candidate
            .matches
            .iter()
            .position(|m| m.existing.id == target)
        else {
            return undecided(
                Kind::InvalidInput,
                format!(
                    "--merge {}: not among this record's reviewed duplicate candidates",
                    target.0
                ),
            );
        };
        // The shared import service never merges into the trash (no implicit restore), so refuse
        // here with the reason instead of failing at commit.
        if candidate.matches[match_index].existing.deleted_at.is_some() {
            return undecided(
                Kind::Conflict,
                format!(
                    "--merge {}: that paper is in the trash; restore it first (merging never restores)",
                    target.0
                ),
            );
        }
        let differing: Vec<String> = candidate.matches[match_index]
            .differences
            .iter()
            .map(|d| d.field.clone())
            .collect();
        let fields = match &policy.merge_fields {
            MergeFields::None => Vec::new(),
            MergeFields::All => differing,
            MergeFields::Only(list) => {
                if let Some(bad) = list.iter().find(|f| !differing.contains(f)) {
                    return undecided(
                        Kind::InvalidInput,
                        format!(
                            "--merge-fields {bad}: not a differing field (choose from: {})",
                            differing.join(", ")
                        ),
                    );
                }
                list.clone()
            }
        };
        return Decision::Act {
            action: ImportAction::Merge {
                match_index,
                fields,
            },
            label: "merge",
        };
    }
    let (persisted, batch) = exact(candidate, index, counts);
    if !persisted.is_empty() || !batch.is_empty() {
        return match policy.on_duplicate {
            OnDuplicate::Skip => Decision::Act {
                action: ImportAction::Skip,
                label: "skip_duplicate",
            },
            _ => undecided(
                Kind::Duplicate,
                if persisted.is_empty() {
                    format!(
                        "exact duplicate of earlier rows {batch:?} in this batch; use --on-duplicate skip"
                    )
                } else {
                    format!(
                        "exact identifier duplicate of {}; use --on-duplicate skip or --merge <id>",
                        ids(&persisted)
                    )
                },
            ),
        };
    }
    if !candidate.matches.is_empty() {
        let similar: Vec<PaperId> = candidate.matches.iter().map(|m| m.existing.id).collect();
        return match policy.on_duplicate {
            OnDuplicate::Skip => Decision::Act {
                action: ImportAction::Skip,
                label: "skip_duplicate",
            },
            OnDuplicate::Create => Decision::Act {
                action: ImportAction::Create,
                label: "create",
            },
            OnDuplicate::Ask => undecided(
                Kind::Duplicate,
                format!(
                    "possible duplicate of {}; use --on-duplicate skip|create or --merge <id>",
                    ids(&similar)
                ),
            ),
        };
    }
    Decision::Act {
        action: ImportAction::Create,
        label: "create",
    }
}

fn ids(ids: &[PaperId]) -> String {
    ids.iter()
        .map(|id| id.0.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn match_kind(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Doi => "doi",
        MatchKind::ExternalId => "external_id",
        MatchKind::TitleAuthorYear => "title_author_year",
        MatchKind::SimilarTitle => "similar_title",
    }
}

/// Exact identifiers (DOI and external IDs, as normalized in the preview) of rows that will be
/// created, by global row index. Citation keys are deliberately excluded: the shared service
/// resolves key collisions with a suffix (`_2`), so a repeated key is not the same work.
#[derive(Debug, Default)]
pub struct SeenIdentifiers(std::collections::HashMap<(String, String), usize>);
impl SeenIdentifiers {
    pub fn identifiers(draft: &cm_protocol::import::PaperDraft) -> Vec<(String, String)> {
        let doi = |v: &str| v.trim().to_lowercase();
        let mut ids: Vec<(String, String)> = draft
            .bibliography
            .doi
            .iter()
            .map(|d| ("doi".to_owned(), doi(d)))
            .collect();
        for id in &draft.external_ids {
            let namespace = id.namespace.trim().to_ascii_lowercase();
            let value = if namespace == "doi" {
                doi(&id.value)
            } else {
                id.value.trim().to_owned()
            };
            ids.push((namespace, value));
        }
        ids
    }
    /// Turns a `create` that repeats an identifier of an earlier row into a duplicate
    /// (skipped with `--on-duplicate skip`, otherwise a decision is required).
    pub fn check(
        &mut self,
        candidate: &PreviewCandidate,
        global_index: usize,
        decision: Decision,
        policy: &Policy,
    ) -> Decision {
        let Decision::Act {
            action: ImportAction::Create,
            ..
        } = decision
        else {
            return decision;
        };
        let ids = Self::identifiers(&candidate.draft);
        if let Some(&first) = ids.iter().find_map(|k| self.0.get(k)) {
            return match policy.on_duplicate {
                OnDuplicate::Skip => Decision::Act {
                    action: ImportAction::Skip,
                    label: "skip_duplicate",
                },
                _ => Decision::Undecided {
                    kind: Kind::Duplicate,
                    reason: format!(
                        "exact identifier duplicate of row {first} (an earlier batch); use --on-duplicate skip"
                    ),
                },
            };
        }
        for id in ids {
            self.0.entry(id).or_insert(global_index);
        }
        decision
    }
}

/// JSON projection of one preview row (stable `papers.v1` shape).
pub fn candidate_json(candidate: &PreviewCandidate, index: usize, decision: &Decision) -> Value {
    let draft = &candidate.draft;
    json!({
        "index": index,
        "source_row": draft.source.row,
        "title": draft.title,
        "doi": draft.bibliography.doi,
        "year": draft.bibliography.year,
        "citation_key": draft.bibliography.citation_key,
        "draft": draft_json(draft),
        "matches": candidate.matches.iter().map(|m| json!({
            "paper_id": m.existing.id.0,
            "kind": match_kind(m.kind),
            "exact": m.kind.exact(),
            "title": m.existing.title,
            "deleted": m.existing.deleted_at.is_some(),
            "differences": m.differences.iter().map(|d| json!({
                "field": d.field, "existing": d.before, "incoming": d.after,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "batch_matches": candidate.batch_matches,
        "diagnostics": candidate.diagnostics.iter().map(|d| json!({
            "field": d.field, "message": d.message, "error": d.error,
        })).collect::<Vec<_>>(),
        "decision": decision_json(decision),
    })
}

pub fn draft_json(draft: &cm_protocol::import::PaperDraft) -> Value {
    json!({
        "title": draft.title,
        "external_ids": draft.external_ids,
        "bibliography": draft.bibliography,
    })
}

pub fn decision_json(decision: &Decision) -> Value {
    match decision {
        Decision::Act { action, label } => {
            let mut v = json!({ "action": label });
            if let ImportAction::Merge { fields, .. } = action {
                v["fields"] = json!(fields);
            }
            v
        }
        Decision::Undecided { kind, reason } => {
            json!({ "action": "needs_decision", "code": kind.code(), "reason": reason })
        }
    }
}

/// The first undecided row as an error kind/message, if any.
pub fn first_undecided(decisions: &[Decision]) -> Option<CliError> {
    decisions.iter().find_map(|d| match d {
        Decision::Undecided { kind, reason } => Some(CliError::new(*kind, reason.clone())),
        Decision::Act { .. } => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_protocol::{
        domain::Paper,
        import::{DuplicateCandidate, FieldDiff, PaperDraft, RowDiagnostic},
    };

    fn paper(n: u128) -> Paper {
        serde_json::from_value(json!({
            "id": format!("00000000-0000-7000-8000-{n:012x}"),
            "title": "Existing",
            "external_ids": [],
        }))
        .unwrap()
    }
    fn candidate(kinds: &[MatchKind]) -> PreviewCandidate {
        PreviewCandidate {
            draft: PaperDraft::titled("Incoming"),
            existing_match: None,
            matches: kinds
                .iter()
                .enumerate()
                .map(|(i, &kind)| DuplicateCandidate {
                    existing: paper(i as u128 + 1),
                    kind,
                    differences: vec![FieldDiff {
                        field: "title".into(),
                        before: json!("Existing"),
                        after: json!("Incoming"),
                    }],
                })
                .collect(),
            batch_matches: vec![],
            diagnostics: vec![],
        }
    }
    fn policy(on_duplicate: OnDuplicate) -> Policy {
        Policy {
            on_duplicate,
            merge: None,
            merge_fields: MergeFields::None,
            skip_invalid: false,
        }
    }
    fn action(d: Decision) -> ImportAction {
        match d {
            Decision::Act { action, .. } => action,
            Decision::Undecided { reason, .. } => panic!("undecided: {reason}"),
        }
    }

    #[test]
    fn new_records_are_created_and_duplicates_never_merge_implicitly() {
        assert_eq!(
            action(decide(&candidate(&[]), 0, &policy(OnDuplicate::Ask))),
            ImportAction::Create
        );
        for kinds in [&[MatchKind::Doi][..], &[MatchKind::SimilarTitle][..]] {
            assert!(matches!(
                decide(&candidate(kinds), 0, &policy(OnDuplicate::Ask)),
                Decision::Undecided {
                    kind: Kind::Duplicate,
                    ..
                }
            ));
            assert_eq!(
                action(decide(&candidate(kinds), 0, &policy(OnDuplicate::Skip))),
                ImportAction::Skip
            );
        }
        // Exact identifier duplicates can never be created twice.
        assert!(matches!(
            decide(
                &candidate(&[MatchKind::Doi]),
                0,
                &policy(OnDuplicate::Create)
            ),
            Decision::Undecided { .. }
        ));
        assert_eq!(
            action(decide(
                &candidate(&[MatchKind::TitleAuthorYear]),
                0,
                &policy(OnDuplicate::Create)
            )),
            ImportAction::Create
        );
    }

    #[test]
    fn merge_needs_an_explicit_reviewed_target_and_fields() {
        let row = candidate(&[MatchKind::SimilarTitle, MatchKind::Doi]);
        let target = row.matches[1].existing.id;
        let mut p = policy(OnDuplicate::Ask);
        p.merge = Some(target);
        assert_eq!(
            action(decide(&row, 0, &p)),
            ImportAction::Merge {
                match_index: 1,
                fields: vec![]
            }
        );
        p.merge_fields = MergeFields::All;
        assert_eq!(
            action(decide(&row, 0, &p)),
            ImportAction::Merge {
                match_index: 1,
                fields: vec!["title".into()]
            }
        );
        p.merge_fields = MergeFields::parse(Some("year"));
        assert!(matches!(
            decide(&row, 0, &p),
            Decision::Undecided {
                kind: Kind::InvalidInput,
                ..
            }
        ));
        p.merge = Some(paper(99).id);
        assert!(matches!(
            decide(&row, 0, &p),
            Decision::Undecided {
                kind: Kind::InvalidInput,
                ..
            }
        ));
        // A trashed target is refused up front (the service never merges into the trash).
        let mut trashed = candidate(&[MatchKind::Doi]);
        trashed.matches[0].existing.deleted_at = Some("2026-01-01T00:00:00Z".into());
        p.merge = Some(trashed.matches[0].existing.id);
        p.merge_fields = MergeFields::None;
        assert!(matches!(
            decide(&trashed, 0, &p),
            Decision::Undecided {
                kind: Kind::Conflict,
                ..
            }
        ));
    }

    #[test]
    fn batch_duplicates_and_invalid_rows() {
        let mut later = candidate(&[]);
        later.batch_matches = vec![0];
        assert!(matches!(
            decide(&later, 1, &policy(OnDuplicate::Create)),
            Decision::Undecided { .. }
        ));
        assert_eq!(
            action(decide(&later, 1, &policy(OnDuplicate::Skip))),
            ImportAction::Skip
        );
        // The first occurrence is not a duplicate of a later row.
        let mut first = candidate(&[]);
        first.batch_matches = vec![1];
        assert_eq!(
            action(decide(&first, 0, &policy(OnDuplicate::Ask))),
            ImportAction::Create
        );

        let mut invalid = candidate(&[]);
        invalid.diagnostics.push(RowDiagnostic {
            field: "doi".into(),
            message: "bad".into(),
            error: true,
        });
        assert!(matches!(
            decide(&invalid, 0, &policy(OnDuplicate::Ask)),
            Decision::Undecided {
                kind: Kind::Unresolved,
                ..
            }
        ));
        let mut bulk = policy(OnDuplicate::Ask);
        bulk.skip_invalid = true;
        assert_eq!(action(decide(&invalid, 0, &bulk)), ImportAction::Skip);
    }

    #[test]
    fn skipped_rows_do_not_make_later_batch_rows_duplicates() {
        // Row 0 is invalid (skipped), row 1 repeats its DOI: row 1 is the first row that is
        // actually saved, so it is created, not skipped as a duplicate of row 0.
        let mut invalid = candidate(&[]);
        invalid.batch_matches = vec![1];
        invalid.diagnostics.push(RowDiagnostic {
            field: "title".into(),
            message: "missing".into(),
            error: true,
        });
        let mut valid = candidate(&[]);
        valid.batch_matches = vec![0];
        let mut bulk = policy(OnDuplicate::Skip);
        bulk.skip_invalid = true;
        let decisions = decide_batch(&[invalid.clone(), valid.clone()], &bulk);
        assert_eq!(action(decisions[0].clone()), ImportAction::Skip);
        assert_eq!(action(decisions[1].clone()), ImportAction::Create);
        // A third row repeating the same identifier is a duplicate of the created row 1.
        let mut third = candidate(&[]);
        third.batch_matches = vec![0, 1];
        let decisions = decide_batch(&[invalid, valid, third], &bulk);
        assert_eq!(action(decisions[2].clone()), ImportAction::Skip);
        assert!(matches!(
            decisions[2],
            Decision::Act {
                label: "skip_duplicate",
                ..
            }
        ));
    }

    #[test]
    fn identifiers_repeated_in_a_later_batch_are_duplicates() {
        let mut row = candidate(&[]);
        row.draft.bibliography.doi = Some("10.1/ABC".into());
        let mut later = candidate(&[]);
        later
            .draft
            .external_ids
            .push(cm_protocol::domain::ExternalId {
                namespace: "DOI".into(),
                value: "10.1/abc".into(),
            });
        let mut other = candidate(&[]);
        other.draft.bibliography.doi = Some("10.1/other".into());
        for (on_duplicate, expect_skip) in [
            (OnDuplicate::Ask, false),
            (OnDuplicate::Create, false),
            (OnDuplicate::Skip, true),
        ] {
            let p = policy(on_duplicate);
            let mut seen = SeenIdentifiers::default();
            assert_eq!(
                action(seen.check(&row, 0, decide(&row, 0, &p), &p)),
                ImportAction::Create
            );
            let repeated = seen.check(&later, 100, decide(&later, 0, &p), &p);
            if expect_skip {
                assert_eq!(action(repeated), ImportAction::Skip);
            } else {
                assert!(matches!(
                    repeated,
                    Decision::Undecided {
                        kind: Kind::Duplicate,
                        ..
                    }
                ));
            }
            assert_eq!(
                action(seen.check(&other, 101, decide(&other, 0, &p), &p)),
                ImportAction::Create
            );
        }
    }
}
