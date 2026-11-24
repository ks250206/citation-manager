//! #19 annotation rows in SQLite: carry-over writes all copies in one transaction and a second
//! run copies nothing.
use cm_core::{
    annotations::*,
    geometry::{Quad, Rect},
    papers::{NewPaper, PaperLibrary},
};
use cm_domain::AttachmentId;
use cm_storage::{LibraryDb, LibraryDirectory};
use rusqlite::{Connection, params};
use uuid::Uuid;

fn quad() -> Quad {
    Rect {
        left: 10.0,
        bottom: 10.0,
        right: 50.0,
        top: 20.0,
    }
    .quad()
}
fn attachment(raw: &Connection, paper: &str, n: u8) -> AttachmentId {
    let id = Uuid::now_v7();
    raw.execute(
        "INSERT INTO attachments(id,paper_id,file_hash,relative_path,original_name,mime_type,byte_size,added_at)
         VALUES (?1,?2,?3,?4,'a.pdf','application/pdf',123,'2026-11-24T00:00:00Z')",
        params![id.to_string(), paper, vec![n; 32], format!("fixture/{n}")],
    )
    .unwrap();
    AttachmentId(id)
}
fn highlight(attachment_id: AttachmentId, quote: &str) -> NewAnnotation {
    NewAnnotation {
        attachment_id,
        kind: AnnotationKind::Highlight,
        source: Source::Human,
        anchor: Anchor::Resolved {
            page_index: 0,
            page_label: None,
            quads: vec![quad()],
        },
        quote: quote.into(),
        context_before: None,
        context_after: None,
        color: Some("yellow".into()),
        comment: None,
    }
}

#[test]
fn carry_over_is_one_transaction_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let dir =
        LibraryDirectory::prepare(temp.path().canonicalize().unwrap().join("library")).unwrap();
    let mut db = LibraryDb::open(&dir).unwrap();
    let paper = db.papers().create(NewPaper::titled("Annotated")).unwrap();
    let raw = Connection::open(dir.database_path()).unwrap();
    raw.pragma_update(None, "foreign_keys", true).unwrap();
    let old = attachment(&raw, &paper.id.0.to_string(), 1);
    let new = attachment(&raw, &paper.id.0.to_string(), 2);

    let mut service = AnnotationService::new(db.annotations());
    let first = match service.create(highlight(old, "first")).unwrap() {
        Edit::Created(a) => a,
        _ => unreachable!(),
    };
    service.create(highlight(old, "second")).unwrap();
    assert_eq!(service.carry_over(old, new).unwrap().len(), 2);
    assert!(service.carry_over(old, new).unwrap().is_empty());
    assert_eq!(service.list(new).unwrap().len(), 2);
    drop(service);

    // All or nothing: a batch whose second row collides writes neither row.
    let mut repo = db.annotations();
    let mut fresh = first.clone();
    fresh.id = AnnotationId(Uuid::now_v7());
    fresh.attachment_id = new;
    let before = repo.for_attachment(new).unwrap().len();
    assert!(repo.insert_all(&[fresh.clone(), first.clone()]).is_err());
    assert_eq!(repo.for_attachment(new).unwrap().len(), before);
    assert!(repo.get(fresh.id).unwrap().is_none());
}
