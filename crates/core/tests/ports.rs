//! Compile and exercise caller-facing ports with substitutes, without any adapter framework.
use cm_core::{ServiceError, import::*, ports::*};
use cm_domain::*;
use std::cell::RefCell;

struct ImportSpy(RefCell<Vec<&'static str>>);
impl ImportService for ImportSpy {
    fn preview(&mut self, request: ImportRequest) -> Result<ImportPreview, ServiceError> {
        self.0.borrow_mut().push("preview");
        Ok(ImportPreview {
            id: PreviewId("opaque-preview".into()),
            candidates: request
                .candidates
                .into_iter()
                .map(|draft| PreviewCandidate {
                    draft,
                    existing_match: None,
                })
                .collect(),
            warnings: vec![],
        })
    }
    fn commit(&mut self, request: ImportCommit) -> Result<ImportOutcome, ServiceError> {
        self.0.borrow_mut().push("commit");
        assert_eq!(request.preview_id, PreviewId("opaque-preview".into()));
        assert_eq!(request.decisions[0].action, ImportAction::Skip);
        Ok(ImportOutcome {
            created: vec![],
            skipped: 1,
        })
    }
}

#[test]
fn an_adapter_can_preview_then_explicitly_skip_without_transport_or_persisted_paper() {
    // A spy verifies separate calls/DTO routing, not a production import implementation.
    let mut service = ImportSpy(RefCell::new(vec![]));
    let request = ImportRequest {
        candidates: vec![PaperDraft {
            title: "論文".into(),
            external_ids: vec![ExternalId {
                namespace: "pmid".into(),
                value: "12345678".into(),
            }],
        }],
        destination: ImportDestination::default(),
    };
    let preview = service.preview(request).unwrap();
    assert_eq!(*service.0.borrow(), vec!["preview"]);
    assert_eq!(
        preview.candidates[0].draft.external_ids[0].namespace,
        "pmid"
    );
    let result = service
        .commit(ImportCommit {
            preview_id: preview.id,
            decisions: vec![ImportDecision {
                candidate_index: 0,
                action: ImportAction::Skip,
            }],
        })
        .unwrap();
    assert_eq!(*service.0.borrow(), vec!["preview", "commit"]);
    assert_eq!(result.skipped, 1);
    assert!(result.created.is_empty());
}

struct MemoryPaper(Option<Paper>);
impl PaperRepository for MemoryPaper {
    fn get(&self, id: PaperId) -> Result<Option<Paper>, ServiceError> {
        Ok(self.0.as_ref().filter(|p| p.id == id).cloned())
    }
    fn save(&mut self, paper: &Paper) -> Result<(), ServiceError> {
        self.0 = Some(paper.clone());
        Ok(())
    }
}

#[test]
fn repository_port_is_replaceable_without_sqlite_and_retains_external_ids() {
    let paper: Paper = serde_json::from_str(
        r#"{"id":"019a5665-18c0-7000-8000-000000000001",
        "title":"sample","external_ids":[{"namespace":"doi","value":"10.1234/a"}]}"#,
    )
    .unwrap();
    let mut repo: Box<dyn PaperRepository> = Box::new(MemoryPaper(None));
    assert_eq!(repo.get(paper.id).unwrap(), None);
    repo.save(&paper).unwrap();
    assert_eq!(repo.get(paper.id).unwrap(), Some(paper));
}
