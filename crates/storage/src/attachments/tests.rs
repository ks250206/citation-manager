use super::*;
use cm_core::{
    attachments::AttachmentService,
    papers::{NewPaper, PaperLibrary},
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn setup() -> (
    tempfile::TempDir,
    LibraryDirectory,
    LibraryDb,
    ImportAttachment,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let directory = LibraryDirectory::prepare(root.join("library")).unwrap();
    let mut db = LibraryDb::open(&directory).unwrap();
    let paper = db
        .papers()
        .create(NewPaper::titled("Retained metadata"))
        .unwrap();
    let source = root.join("original.pdf");
    fs::write(&source, vec![b'x'; BUFFER_SIZE * 3 + 7]).unwrap();
    let request = ImportAttachment {
        paper_id: paper.id,
        source,
        original_name: "original.pdf".into(),
        media_type: "application/pdf".into(),
        expected_sha256: None,
    };
    (temp, directory, db, request)
}
fn digest(path: &std::path::Path) -> [u8; 32] {
    Sha256::digest(fs::read(path).unwrap()).into()
}

#[test]
fn interruptions_retain_old_associations_and_are_detectable_and_retryable() {
    for cut in [Cut::Copy, Cut::AfterPublish, Cut::BeforeCommit, Cut::Commit] {
        let (_temp, directory, mut db, mut request) = setup();
        let source_hash = digest(&request.source);
        let old = AttachmentService::new(db.managed_attachments(&directory).unwrap())
            .import(request.clone())
            .unwrap();
        let old_path = directory.path().join(old.storage_key.as_str());
        let old_hash = digest(&old_path);
        // A new immutable source supplies a different blob; the first original stays untouched.
        let newer = request.source.with_file_name("newer.pdf");
        fs::write(&newer, vec![b'y'; BUFFER_SIZE * 3]).unwrap();
        request.source = newer.clone();
        let new_hash = digest(&newer);
        assert!(
            db.managed_attachments(&directory)
                .unwrap()
                .ingest(request.clone(), cut)
                .is_err()
        );
        assert_eq!(digest(&old_path), old_hash);
        assert_eq!(
            digest(&request.source.with_file_name("original.pdf")),
            source_hash
        );
        assert_eq!(digest(&newer), new_hash);
        assert_eq!(
            db.attachments().for_paper(request.paper_id).unwrap(),
            vec![old.clone()]
        );
        let report = db.check_integrity(&directory).unwrap();
        assert!(report.dangling.is_empty() && report.corrupt.is_empty());
        if cut == Cut::Copy {
            assert_eq!(report.partials.len(), 1);
            assert!(report.orphans.is_empty());
        } else {
            assert_eq!(report.orphans.len(), 1);
            assert!(report.partials.is_empty());
        }
        assert_eq!(
            db.connection
                .query_row("SELECT count(*) FROM paper_tags", [], |r| r
                    .get::<_, u32>(0))
                .unwrap(),
            0
        );
        drop(db);
        let mut db = LibraryDb::open(&directory).unwrap();
        let new = AttachmentService::new(db.managed_attachments(&directory).unwrap())
            .import(request.clone())
            .unwrap();
        assert_eq!(new.sha256, new_hash);
        assert_eq!(
            db.attachments().for_paper(request.paper_id).unwrap().len(),
            2
        );
        let after = db.check_integrity(&directory).unwrap();
        assert!(after.orphans.is_empty() && after.dangling.is_empty() && after.corrupt.is_empty());
        assert_eq!(after.partials.len(), usize::from(cut == Cut::Copy));
        assert_eq!(digest(&old_path), old_hash);
    }
}

#[test]
fn shared_blob_survives_association_failure_and_soft_delete() {
    let (_temp, directory, mut db, mut request) = setup();
    let first = db
        .managed_attachments(&directory)
        .unwrap()
        .import(request.clone())
        .unwrap();
    let second_paper = db.papers().create(NewPaper::titled("second")).unwrap();
    request.paper_id = second_paper.id;
    let second = db
        .managed_attachments(&directory)
        .unwrap()
        .import(request.clone())
        .unwrap();
    assert_eq!(first.storage_key, second.storage_key);
    assert_ne!(first.id, second.id);
    request.paper_id = cm_domain::PaperId(Uuid::now_v7());
    assert!(
        db.managed_attachments(&directory)
            .unwrap()
            .import(request.clone())
            .is_err()
    );
    request.paper_id = second_paper.id;
    assert!(
        db.managed_attachments(&directory)
            .unwrap()
            .ingest(request.clone(), Cut::Commit)
            .is_err()
    );
    assert_eq!(
        hash(File::open(directory.path().join(first.storage_key.as_str())).unwrap()).unwrap(),
        (first.sha256, first.byte_size)
    );
    db.papers().set_deleted(first.paper_id, true).unwrap();
    assert!(db.check_integrity(&directory).unwrap().is_clean());
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .status(second.id)
            .unwrap(),
        AttachmentStatus::Present
    );
    assert_eq!(
        db.attachments().for_paper(second_paper.id).unwrap(),
        vec![second]
    );
}

#[test]
fn corruption_missing_and_expected_hash_errors_preserve_original_and_are_typed() {
    let (_temp, directory, mut db, mut request) = setup();
    let original = digest(&request.source);
    request.expected_sha256 = Some([0; 32]);
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .import(request.clone())
            .unwrap_err(),
        AttachmentError::HashMismatch
    );
    assert_eq!(db.check_integrity(&directory).unwrap().partials.len(), 1);
    request.expected_sha256 = Some(original);
    let attachment = db
        .managed_attachments(&directory)
        .unwrap()
        .import(request.clone())
        .unwrap();
    let path = directory.path().join(attachment.storage_key.as_str());
    fs::write(&path, b"corrupt").unwrap();
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .status(attachment.id)
            .unwrap(),
        AttachmentStatus::Corrupt
    );
    assert_eq!(db.check_integrity(&directory).unwrap().corrupt.len(), 1);
    assert!(
        db.managed_attachments(&directory)
            .unwrap()
            .import(request.clone())
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"corrupt");
    fs::remove_file(&path).unwrap();
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .status(attachment.id)
            .unwrap(),
        AttachmentStatus::Missing
    );
    assert_eq!(db.check_integrity(&directory).unwrap().dangling.len(), 1);
    let repaired = db
        .managed_attachments(&directory)
        .unwrap()
        .import(request.clone())
        .unwrap();
    assert_eq!(repaired.sha256, original);
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .status(attachment.id)
            .unwrap(),
        AttachmentStatus::Present
    );
    assert_eq!(digest(&request.source), original);
}

#[cfg(unix)]
#[test]
fn source_and_store_traps_are_rejected_without_changing_targets() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (_temp, directory, mut db, request) = setup();
    let original = digest(&request.source);
    for variant in ["symlink", "hardlink", "public", "directory"] {
        let attachment = db
            .managed_attachments(&directory)
            .unwrap()
            .import(request.clone())
            .unwrap();
        let path = directory.path().join(attachment.storage_key.as_str());
        fs::remove_file(&path).unwrap();
        match variant {
            "symlink" => symlink(&request.source, &path).unwrap(),
            "hardlink" => fs::hard_link(&request.source, &path).unwrap(),
            "public" => {
                fs::write(&path, b"unsafe mode").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => fs::create_dir(&path).unwrap(),
        }
        assert_eq!(
            db.managed_attachments(&directory)
                .unwrap()
                .status(attachment.id)
                .unwrap_err(),
            AttachmentError::UnsafePath
        );
        assert!(
            db.managed_attachments(&directory)
                .unwrap()
                .import(request.clone())
                .is_err()
        );
        assert!(
            !db.check_integrity(&directory)
                .unwrap()
                .unsafe_entries
                .is_empty()
        );
        if variant == "directory" {
            fs::remove_dir(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        assert_eq!(digest(&request.source), original);
    }
    let mut alias_request = request.clone();
    let alias = request.source.with_file_name("alias.pdf");
    symlink(&request.source, &alias).unwrap();
    alias_request.source = alias;
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .import(alias_request)
            .unwrap_err(),
        AttachmentError::UnsafePath
    );
    for input in [
        PathBuf::from("relative"),
        request.source.parent().unwrap().join("./original.pdf"),
        request.source.parent().unwrap().join("../original.pdf"),
    ] {
        let mut bad = request.clone();
        bad.source = input;
        assert_eq!(
            db.managed_attachments(&directory)
                .unwrap()
                .import(bad)
                .unwrap_err(),
            AttachmentError::UnsafePath
        );
    }
    let other =
        LibraryDirectory::prepare(request.source.parent().unwrap().join("other-library")).unwrap();
    assert!(db.managed_attachments(&other).is_err());
    let attachment = db
        .managed_attachments(&directory)
        .unwrap()
        .import(request.clone())
        .unwrap();
    let shard = directory
        .path()
        .join(attachment.storage_key.as_str())
        .parent()
        .unwrap()
        .to_owned();
    let moved = shard.with_extension("moved");
    fs::rename(&shard, &moved).unwrap();
    symlink(&moved, &shard).unwrap();
    assert_eq!(
        db.managed_attachments(&directory)
            .unwrap()
            .status(attachment.id)
            .unwrap_err(),
        AttachmentError::UnsafePath
    );
    assert!(
        db.managed_attachments(&directory)
            .unwrap()
            .import(request)
            .is_err()
    );
}

#[test]
fn lost_response_after_commit_keeps_durable_new_and_old_references_on_reopen() {
    let (_temp, directory, mut db, mut request) = setup();
    let old = db
        .managed_attachments(&directory)
        .unwrap()
        .import(request.clone())
        .unwrap();
    let old_path = directory.path().join(old.storage_key.as_str());
    let original = digest(&request.source);
    let newer = request.source.with_file_name("new.pdf");
    fs::write(&newer, b"second version").unwrap();
    request.source = newer;
    assert!(
        db.managed_attachments(&directory)
            .unwrap()
            .ingest(request.clone(), Cut::AfterCommit)
            .is_err()
    );
    drop(db);
    let db = LibraryDb::open(&directory).unwrap();
    let references = db.attachments().for_paper(request.paper_id).unwrap();
    assert_eq!(references.len(), 2);
    assert!(references.contains(&old));
    assert!(db.check_integrity(&directory).unwrap().is_clean());
    assert_eq!(digest(&old_path), original);
    assert_eq!(
        digest(&request.source.with_file_name("original.pdf")),
        original
    );
}
