#![cfg(unix)]
use cm_storage::{LibraryDb, LibraryDirectory, StorageError};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

fn test_root() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    (temp, path)
}
fn mode(path: &std::path::Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

#[test]
fn creates_only_explicit_private_library_and_private_database_sidecars() {
    let (_temp, root) = test_root();
    let path = root.join("parent/library");
    let dir = LibraryDirectory::prepare(&path).unwrap();
    assert_eq!(mode(&path), 0o700);
    assert_eq!(mode(&root.join("parent")), 0o700);
    let db = LibraryDb::open(&dir).unwrap();
    for file in ["library.sqlite", "library.sqlite-wal", "library.sqlite-shm"] {
        let path = path.join(file);
        assert_eq!(mode(&path), 0o600);
        let m = fs::metadata(path).unwrap();
        assert_eq!(m.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(m.nlink(), 1);
    }
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    drop(db);
    assert!(LibraryDb::open(&dir).is_ok());
}

#[test]
fn rejects_relative_traversal_symlink_and_public_directory_without_chmod() {
    let (_temp, root) = test_root();
    for path in [
        std::path::PathBuf::from("relative"),
        root.join("../escape"),
        root.join("./dot"),
    ] {
        assert!(matches!(
            LibraryDirectory::prepare(path),
            Err(StorageError::UnsafePath(_))
        ));
    }
    let public = root.join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(LibraryDirectory::prepare(&public).is_err());
    assert_eq!(mode(&public), 0o755);
    let writable = root.join("writable");
    fs::create_dir(&writable).unwrap();
    fs::set_permissions(&writable, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(LibraryDirectory::prepare(writable.join("new-library")).is_err());
    assert!(!writable.join("new-library").exists());
    let own = LibraryDirectory::prepare(root.join("own")).unwrap();
    let alias = root.join("alias");
    symlink(own.path(), &alias).unwrap();
    assert!(LibraryDirectory::prepare(&alias).is_err());
    assert!(LibraryDirectory::prepare(alias.join("nested")).is_err());
    assert!(!own.path().join("nested").exists());
    let plain_file = root.join("file");
    fs::write(&plain_file, b"keep").unwrap();
    assert!(LibraryDirectory::prepare(&plain_file).is_err());
    assert_eq!(fs::read(&plain_file).unwrap(), b"keep");
}

#[test]
fn rejects_database_symlinks_hardlinks_insecure_modes_and_sidecar_traps() {
    for name in [
        "library.sqlite",
        "library.sqlite-wal",
        "library.sqlite-shm",
        "library.sqlite-journal",
    ] {
        let (_temp, root) = test_root();
        let target = root.join("untouched");
        fs::write(&target, b"private original").unwrap();
        let dir = LibraryDirectory::prepare(root.join("library")).unwrap();
        symlink(&target, dir.path().join(name)).unwrap();
        assert!(matches!(
            LibraryDb::open(&dir),
            Err(StorageError::UnsafePath(_))
        ));
        assert_eq!(fs::read(&target).unwrap(), b"private original");
        if name != "library.sqlite" {
            assert!(!dir.database_path().exists());
        }
    }
    let (_temp, root) = test_root();
    let dir = LibraryDirectory::prepare(root.join("library")).unwrap();
    let db = LibraryDb::open(&dir).unwrap();
    drop(db);
    let link = root.join("hard-link");
    fs::hard_link(dir.database_path(), link).unwrap();
    assert!(matches!(
        LibraryDb::open(&dir),
        Err(StorageError::UnsafePath(_))
    ));
    let (_temp, root) = test_root();
    let dir = LibraryDirectory::prepare(root.join("library")).unwrap();
    drop(LibraryDb::open(&dir).unwrap());
    let original = fs::read(dir.database_path()).unwrap();
    fs::set_permissions(dir.database_path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        LibraryDb::open(&dir),
        Err(StorageError::UnsafePath(_))
    ));
    assert_eq!(mode(&dir.database_path()), 0o644);
    assert_eq!(fs::read(dir.database_path()).unwrap(), original);
    fs::set_permissions(dir.database_path(), fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(dir.path().join("library.sqlite-shm")).unwrap();
    assert!(matches!(
        LibraryDb::open(&dir),
        Err(StorageError::UnsafePath(_))
    ));
}

#[test]
fn reopening_revalidates_directory_permissions() {
    let (_temp, root) = test_root();
    let dir = LibraryDirectory::prepare(root.join("library")).unwrap();
    drop(LibraryDb::open(&dir).unwrap());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).unwrap();
    assert!(matches!(
        LibraryDb::open(&dir),
        Err(StorageError::UnsafePath(_))
    ));
    assert_eq!(mode(dir.path()), 0o750);
}
