use super::*;
use crate::{ServiceError, ports::ImportService};
use normalize::{invalid, *};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct ImportPolicy {
    pub lifetime: Duration,
    pub max_previews: usize,
}
impl Default for ImportPolicy {
    fn default() -> Self {
        Self {
            lifetime: Duration::from_secs(300),
            max_previews: 4,
        }
    }
}
struct Retained {
    plan: PreparedImport,
    expires: Instant,
}
pub struct ImportCoordinator<B> {
    backend: B,
    policy: ImportPolicy,
    retained: HashMap<PreviewId, Retained>,
}
impl<B: ImportBackend> ImportCoordinator<B> {
    pub fn new(backend: B) -> Self {
        Self::with_policy(backend, ImportPolicy::default())
    }
    pub fn with_policy(backend: B, mut policy: ImportPolicy) -> Self {
        policy.max_previews = policy.max_previews.clamp(1, 4);
        policy.lifetime = policy.lifetime.min(Duration::from_secs(300));
        Self {
            backend,
            policy,
            retained: HashMap::new(),
        }
    }
    pub fn into_backend(self) -> B {
        self.backend
    }
    pub fn cancel(&mut self, id: &PreviewId) -> Result<(), ServiceError> {
        self.prune();
        self.retained
            .remove(id)
            .map(|_| ())
            .ok_or(ServiceError::NotFound)
    }
    fn prune(&mut self) {
        self.retained.retain(|_, v| v.expires > Instant::now());
    }
}
impl<B: ImportBackend> ImportService for ImportCoordinator<B> {
    fn preview(&mut self, request: ImportRequest) -> Result<ImportPreview, ServiceError> {
        self.prune();
        if self.retained.len() >= self.policy.max_previews {
            return Err(invalid(
                "preview capacity reached; cancel or expire previews",
            ));
        }
        if request.candidates.is_empty() || request.candidates.len() > MAX_BATCH {
            return Err(invalid("import batch must contain 1..=100 rows"));
        }
        // Resource limits fail the request before any inventory work; malformed metadata
        // becomes a per-row diagnostic so valid rows remain reviewable.
        for d in &request.candidates {
            validate_limits(d)?;
        }
        let snapshot = self.backend.snapshot(&request.destination)?;
        check_snapshot(&snapshot)?;
        let id = PreviewId(format!("import-{}", uuid::Uuid::now_v7()));
        let candidates = build_preview(&request.candidates, &snapshot.papers)?;
        let preview = ImportPreview { id: id.clone(), candidates, warnings:vec!["Commit requires one decision per row; fuzzy matches are suggestions. Handles are service-local, not credentials.".into()] };
        let plan = PreparedImport {
            revision: snapshot.revision,
            preview: preview.clone(),
            destination: request.destination,
            decisions: vec![],
        };
        self.retained.insert(
            id,
            Retained {
                plan,
                expires: Instant::now() + self.policy.lifetime,
            },
        );
        Ok(preview)
    }
    fn commit(&mut self, request: ImportCommit) -> Result<ImportOutcome, ServiceError> {
        self.prune();
        // Never retain an oversized, invalid client decision payload in preview state.
        if request.decisions.len() > MAX_BATCH
            || request.decisions.iter().any(|d| {
                matches!(&d.action, ImportAction::Merge { fields, .. }
                if fields.len() > 32 || fields.iter().any(|f| f.len() > 128))
            })
        {
            return Err(invalid("decision payload exceeds import budget"));
        }
        let retained = self
            .retained
            .get_mut(&request.preview_id)
            .ok_or(ServiceError::NotFound)?;
        retained.plan.decisions = request.decisions;
        if let Err(error) = validate_decisions(&retained.plan) {
            retained.plan.decisions.clear();
            return Err(error);
        }
        // Validation failure retains the preview for corrected decisions. Write failures
        // may invalidate the adapter revision and require a fresh preview.
        // Conflict requires a fresh preview; success consumes the handle (replay NotFound).
        let outcome = self.backend.apply(&retained.plan)?;
        self.retained.remove(&request.preview_id);
        Ok(outcome)
    }
    fn cancel(&mut self, id: &PreviewId) -> Result<(), ServiceError> {
        ImportCoordinator::cancel(self, id)
    }
}
fn build_preview(
    drafts: &[PaperDraft],
    papers: &[Paper],
) -> Result<Vec<PreviewCandidate>, ServiceError> {
    let mut used = papers
        .iter()
        .filter_map(|p| p.bibliography.citation_key.clone())
        .collect::<HashSet<_>>();
    let mut rows = Vec::new();
    let mut retained_bytes = 0;
    for input in drafts {
        let mut diagnostics = input.diagnostics.clone();
        let draft = match normalize(input) {
            Ok(mut d) => {
                let exact = papers
                    .iter()
                    .filter(|paper| {
                        dedup::classify(&d, &paper_draft(paper)).is_some_and(MatchKind::exact)
                    })
                    .collect::<Vec<_>>();
                let own_key = d.bibliography.citation_key.as_ref().filter(|key| {
                    exact.len() == 1 && exact[0].bibliography.citation_key.as_ref() == Some(*key)
                });
                let key = own_key
                    .cloned()
                    .unwrap_or_else(|| citation_keys::allocate(&d, &mut used));
                if d.bibliography
                    .citation_key
                    .as_ref()
                    .is_some_and(|k| k != &key)
                {
                    diagnostics.push(RowDiagnostic {
                        field: "citation_key".into(),
                        message: format!(
                            "Citation key {} collides; proposed {key}",
                            d.bibliography.citation_key.as_deref().unwrap_or_default()
                        ),
                        error: false,
                    });
                }
                d.bibliography.citation_key = Some(key);
                if let Err(error) = validate_limits(&d) {
                    diagnostics.push(RowDiagnostic {
                        field: "citation_key".into(),
                        message: error.to_string(),
                        error: true,
                    });
                }
                d
            }
            Err(error) => {
                diagnostics.push(RowDiagnostic {
                    field: "metadata".into(),
                    message: error.to_string(),
                    error: true,
                });
                input.clone()
            }
        };
        let mut matches = Vec::new();
        if !diagnostics.iter().any(|d| d.error) {
            for p in papers {
                if let Some(kind) = dedup::classify(&draft, &paper_draft(p)) {
                    let differences = dedup::differences(p, &draft);
                    retained_bytes += serde_json::to_vec(p)
                        .map_err(|_| ServiceError::Unavailable)?
                        .len();
                    retained_bytes += differences
                        .iter()
                        .map(|d| {
                            d.before.to_string().len() + d.after.to_string().len() + d.field.len()
                        })
                        .sum::<usize>();
                    if matches.len() >= 20 || retained_bytes > MAX_PREVIEW_BYTES {
                        return Err(invalid("duplicate candidate/preview budget exceeded"));
                    }
                    matches.push(DuplicateCandidate {
                        existing: p.clone(),
                        kind,
                        differences,
                    });
                }
            }
            matches.sort_by_key(|m| match m.kind {
                MatchKind::Doi => 0,
                MatchKind::ExternalId => 1,
                MatchKind::TitleAuthorYear => 2,
                MatchKind::SimilarTitle => 3,
            });
        }
        retained_bytes += serde_json::to_vec(&draft)
            .map_err(|_| ServiceError::Unavailable)?
            .len();
        retained_bytes += diagnostics
            .iter()
            .map(|d| d.field.len() + d.message.len())
            .sum::<usize>();
        if retained_bytes > MAX_PREVIEW_BYTES {
            return Err(invalid("preview byte budget exceeded"));
        }
        rows.push(PreviewCandidate {
            existing_match: matches
                .iter()
                .find(|m| m.kind.exact())
                .map(|m| m.existing.id),
            draft,
            matches,
            batch_matches: vec![],
            diagnostics,
        });
    }
    for i in 0..rows.len() {
        rows[i].batch_matches = (0..rows.len())
            .filter(|&j| {
                j != i
                    && dedup::classify(&rows[i].draft, &rows[j].draft).is_some_and(MatchKind::exact)
            })
            .collect();
    }
    Ok(rows)
}
fn validate_decisions(plan: &PreparedImport) -> Result<(), ServiceError> {
    if plan.decisions.len() != plan.preview.candidates.len() {
        return Err(invalid("every row requires exactly one decision"));
    }
    let mut seen = HashSet::new();
    let mut targets = HashSet::new();
    for d in &plan.decisions {
        let row = plan
            .preview
            .candidates
            .get(d.candidate_index)
            .ok_or_else(|| invalid("candidate index out of range"))?;
        if !seen.insert(d.candidate_index) {
            return Err(invalid("duplicate decision index"));
        }
        if d.action == ImportAction::Skip {
            continue;
        }
        if row.diagnostics.iter().any(|d| d.error) {
            return Err(invalid(
                "invalid row can only be skipped; edit then preview again",
            ));
        }
        match &d.action {
            ImportAction::Create if row.matches.iter().any(|m| m.kind.exact()) => {
                return Err(ServiceError::Conflict);
            }
            ImportAction::Merge {
                match_index,
                fields,
            } => {
                let candidate = row
                    .matches
                    .get(*match_index)
                    .ok_or_else(|| invalid("unreviewed merge target"))?;
                if !targets.insert(candidate.existing.id) {
                    return Err(invalid("multiple rows cannot merge the same paper"));
                }
                if dedup::doi(&row.draft)
                    .zip(dedup::doi(&paper_draft(&candidate.existing)))
                    .is_some_and(|(a, b)| a != b)
                {
                    return Err(invalid(
                        "DOI disagreement; correct metadata and preview again",
                    ));
                }
                dedup::merge(candidate, fields)?;
            }
            _ => {}
        }
    }
    Ok(())
}
impl PreparedImport {
    /// Invoke ONLY inside the adapter's write transaction with a fresh inventory.
    pub fn writes(&self, current: &ImportSnapshot) -> Result<Vec<ImportWrite>, ServiceError> {
        if current.revision != self.revision {
            return Err(ServiceError::Conflict);
        }
        check_snapshot(current)?;
        validate_decisions(self)?;
        let mut writes = Vec::new();
        for d in &self.decisions {
            let row = &self.preview.candidates[d.candidate_index];
            if d.action != ImportAction::Skip && normalize(&row.draft)? != row.draft {
                return Err(ServiceError::Conflict);
            }
            match &d.action {
                ImportAction::Skip => {}
                ImportAction::Create => {
                    writes.push(ImportWrite::Create {
                        index: d.candidate_index,
                        draft: row.draft.clone(),
                    });
                }
                ImportAction::Merge {
                    match_index,
                    fields,
                } => {
                    let candidate = &row.matches[*match_index];
                    if !current.papers.contains(&candidate.existing) {
                        return Err(ServiceError::Conflict);
                    }
                    writes.push(ImportWrite::Merge {
                        index: d.candidate_index,
                        paper: dedup::merge(candidate, fields)?,
                    });
                }
            }
        }
        // Validate the final state, including exact duplicates within the approved batch,
        // all preexisting/trash identifiers and citation-key uniqueness, before SQL writes.
        let replaced = writes
            .iter()
            .filter_map(|w| {
                if let ImportWrite::Merge { paper, .. } = w {
                    Some(paper.id)
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();
        let mut final_drafts = current
            .papers
            .iter()
            .filter(|p| !replaced.contains(&p.id))
            .map(paper_draft)
            .collect::<Vec<_>>();
        let existing_count = final_drafts.len();
        for w in &writes {
            let draft = match w {
                ImportWrite::Create { draft, .. } => draft.clone(),
                ImportWrite::Merge { paper, .. } => paper_draft(paper),
            };
            if final_drafts.iter().any(|p| {
                dedup::classify(p, &draft).is_some_and(MatchKind::exact)
                    || p.bibliography.citation_key.is_some()
                        && p.bibliography.citation_key == draft.bibliography.citation_key
            }) {
                return Err(ServiceError::Conflict);
            }
            final_drafts.push(draft);
        }
        debug_assert_eq!(final_drafts.len(), existing_count + writes.len());
        Ok(writes)
    }
}
