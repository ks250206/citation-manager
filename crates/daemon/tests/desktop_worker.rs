use cm_daemon::desktop_worker::InProcessLibraryClient;
use cm_protocol::library::{
    LibraryClient, LibraryReply, LibraryRequest, NewPaper, PaperList, ServiceError,
};
use std::{sync::mpsc, time::Duration};
fn request(
    client: &dyn LibraryClient,
    request: LibraryRequest,
) -> Result<LibraryReply, ServiceError> {
    let (tx, rx) = mpsc::channel();
    client
        .submit(request, Box::new(move |reply| tx.send(reply).unwrap()))
        .unwrap();
    rx.recv_timeout(Duration::from_secs(10)).unwrap()
}
fn paper(reply: LibraryReply) -> cm_protocol::domain::Paper {
    match reply {
        LibraryReply::Paper(p) => *p,
        _ => panic!("expected paper"),
    }
}
#[test]
fn worker_create_edit_reopen_preserves_unicode_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("library");
    let client = InProcessLibraryClient::start(path.clone()).unwrap();
    let mut input = NewPaper::titled("固体電解質 🙂 e\u{301}");
    input.bibliography.doi = Some("https://doi.org/10.1000/ABC".into());
    input.bibliography.abstract_text = Some("実測と仮説を分ける".into());
    let created = paper(request(&client, LibraryRequest::Create(Box::new(input.clone()))).unwrap());
    assert_eq!(created.bibliography.doi.as_deref(), Some("10.1000/abc"));
    assert_eq!(
        request(&client, LibraryRequest::Create(Box::new(input))),
        Err(ServiceError::Conflict)
    );
    let mut edited = created;
    edited.title.push_str(" / 編集");
    let saved = paper(request(&client, LibraryRequest::Update(Box::new(edited))).unwrap());
    assert_eq!(
        saved.bibliography.abstract_text.as_deref(),
        Some("実測と仮説を分ける")
    );
    // Close the worker and use a separately constructed adapter/connection.
    drop(client);
    let reopened = InProcessLibraryClient::start(path).unwrap();
    assert_eq!(
        paper(request(&reopened, LibraryRequest::Get(saved.id)).unwrap()),
        saved
    );
    assert_eq!(
        request(&reopened, LibraryRequest::List(PaperList::default())).unwrap(),
        LibraryReply::Papers(vec![saved])
    );
}
#[test]
fn worker_reports_open_and_validation_errors_and_completes_on_worker() {
    let client = InProcessLibraryClient::start("relative-is-invalid".into()).unwrap();
    for _ in 0..2 {
        assert!(request(&client, LibraryRequest::List(PaperList::default())).is_err());
    }
    let temp = tempfile::tempdir().unwrap();
    let client =
        InProcessLibraryClient::start(temp.path().canonicalize().unwrap().join("library")).unwrap();
    assert!(matches!(
        request(
            &client,
            LibraryRequest::Create(Box::new(NewPaper::titled(" ")))
        ),
        Err(ServiceError::InvalidInput(_))
    ));
    let (tx, rx) = mpsc::channel();
    let caller = std::thread::current().id();
    client
        .submit(
            LibraryRequest::List(PaperList::default()),
            Box::new(move |result| tx.send((std::thread::current().id(), result)).unwrap()),
        )
        .unwrap();
    let (worker, result) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_ne!(caller, worker);
    assert_eq!(result.unwrap(), LibraryReply::Papers(vec![]));
}
