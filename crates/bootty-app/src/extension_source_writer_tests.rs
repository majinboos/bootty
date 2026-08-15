use std::{fs, io};

use super::*;

#[test]
fn pre_replacement_faults_keep_original_bytes_and_mode() {
    let directory = tempfile::tempdir().expect("module directory");
    let path = directory.path().join("module.luau");
    let original = b"return function() return 'old' end";
    fs::write(&path, original).expect("write original module");

    for fault in [WriteFault::Sync, WriteFault::Replace] {
        let error = save_with_fault(&path, "new", fault).expect_err("fault must fail");
        assert!(error.to_string().contains("injected"));
        assert_eq!(fs::read(&path).expect("read original module"), original);
    }
}

#[cfg(unix)]
#[test]
fn final_symlink_is_resolved_before_lock_and_replacement() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = tempfile::tempdir().expect("module directory");
    let target = directory.path().join("target.luau");
    let link = directory.path().join("module.luau");
    fs::write(&target, "old").expect("write target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).expect("set target mode");
    symlink("target.luau", &link).expect("create module symlink");

    save_with_fault(&link, "new", WriteFault::None).expect("replace through symlink");

    assert!(
        fs::symlink_metadata(&link)
            .expect("stat module symlink")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).expect("read target"), "new");
    assert_eq!(
        fs::metadata(&target)
            .expect("stat target")
            .permissions()
            .mode()
            & 0o7777,
        0o444
    );
}

#[cfg(unix)]
#[test]
fn symlink_cycles_are_rejected_before_lock_creation() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("module directory");
    let first = directory.path().join("first.luau");
    let second = directory.path().join("second.luau");
    symlink("second.luau", &first).expect("create first symlink");
    symlink("first.luau", &second).expect("create second symlink");

    let error = save_with_fault(&first, "new", WriteFault::None).expect_err("cycle must fail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("symlink cycle"));
}
