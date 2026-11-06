use cm_desktop::service_client::LibraryState;
use cm_protocol::{
    domain::{Bibliography, Paper},
    library::{LibraryReply, LibraryRequest, ServiceError},
};
fn paper() -> Paper {
    // Stable synthetic value parsed through the public domain contract.
    serde_json::from_str(r#"{"id":"01900000-0000-7000-8000-000000000001","title":"Original title","external_ids":[]}"#).unwrap()
}
#[test]
fn superseded_responses_and_old_errors_cannot_replace_current_state() {
    let mut state = LibraryState::default();
    let (old, _) = state.list(false).unwrap();
    state.new_paper();
    assert!(!state.complete(old, Ok(LibraryReply::Papers(vec![paper()]))));
    assert!(state.papers.is_empty());
    let (a, _) = state.select(paper().id).unwrap();
    let (b, _) = state.select(paper().id).unwrap();
    assert!(!state.complete(a, Err(ServiceError::Unavailable)));
    assert!(state.loading());
    assert!(state.complete(b, Ok(LibraryReply::Paper(Box::new(paper())))));
    assert!(!state.error);
}
#[test]
fn failed_save_keeps_confirmed_paper_and_success_preserves_unedited_fields() {
    let mut state = LibraryState::default();
    let mut original = paper();
    original.bibliography = Bibliography {
        journal: Some("Retained journal".into()),
        ..original.bibliography
    };
    let (load, _) = state.select(original.id).unwrap();
    state.complete(load, Ok(LibraryReply::Paper(Box::new(original.clone()))));
    let (save, request) = state.save("編集🙂".into(), "詳細".into()).unwrap();
    let LibraryRequest::Update(updated) = request else {
        panic!("update expected")
    };
    assert_eq!(updated.bibliography.journal, original.bibliography.journal);
    assert_eq!(updated.title, "編集🙂");
    assert!(state.list(false).is_none());
    assert!(state.select(original.id).is_none());
    assert!(state.save("double".into(), "".into()).is_none());
    state.new_paper();
    assert!(state.saving());
    state.complete(save, Err(ServiceError::Conflict));
    assert!(state.error);
    assert_eq!(state.selected.as_ref(), Some(&original));
    let (retry, _) = state.save(updated.title.clone(), "詳細".into()).unwrap();
    assert!(state.complete(retry, Ok(LibraryReply::Paper(updated))));
    assert_eq!(state.message, "Saved locally");
    assert_eq!(state.papers[0].title, "編集🙂");
}

#[test]
fn creating_a_new_paper_does_not_skip_unloaded_keyset_pages() {
    let mut state = LibraryState::default();
    let original = paper();
    let (list, _) = state.list(false).unwrap();
    let page: Vec<Paper> = (1..=50)
        .map(|n| {
            let mut row = original.clone();
            row.id =
                serde_json::from_str(&format!("\"01900000-0000-7000-8000-{n:012x}\"")).unwrap();
            row
        })
        .collect();
    let last_loaded = page.last().unwrap().id;
    state.complete(list, Ok(LibraryReply::Papers(page)));
    assert!(state.has_more);
    let mut later = original.clone();
    later.id = serde_json::from_str("\"ffffffff-ffff-7fff-8fff-ffffffffffff\"").unwrap();
    state.new_paper();
    let (save, _) = state.save("Later".into(), "".into()).unwrap();
    state.complete(save, Ok(LibraryReply::Paper(Box::new(later))));
    let (_, LibraryRequest::List(query)) = state.list(true).unwrap() else {
        panic!("list expected")
    };
    assert_eq!(query.after, Some(last_loaded));
}

#[test]
fn a_replacement_client_can_deliver_deferred_errors_without_saved_data() {
    use cm_protocol::library::{Completion, LibraryClient};
    use std::sync::{Mutex, mpsc};
    #[derive(Default)]
    struct DeferredClient(Mutex<Option<Completion>>);
    impl LibraryClient for DeferredClient {
        fn submit(&self, _: LibraryRequest, complete: Completion) -> Result<(), ServiceError> {
            *self.0.lock().unwrap() = Some(complete);
            Ok(())
        }
    }
    let replacement = DeferredClient::default();
    let client: &dyn LibraryClient = &replacement;
    let mut state = LibraryState::default();
    let (id, request) = state.list(false).unwrap();
    let (tx, rx) = mpsc::channel();
    client
        .submit(
            request,
            Box::new(move |reply| {
                let _ = tx.send(reply);
            }),
        )
        .unwrap();
    assert!(state.loading());
    replacement.0.lock().unwrap().take().unwrap()(Err(ServiceError::Unavailable));
    state.complete(id, rx.recv().unwrap());
    assert!(state.error);
    assert!(state.papers.is_empty());
}
