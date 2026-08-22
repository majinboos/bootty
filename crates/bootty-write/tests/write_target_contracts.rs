use std::{fs, io, path::Path};

use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};

#[test]
fn replacement_keeps_one_resolved_target_and_preserves_existing_bytes_until_commit()
-> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("state.txt");
    fs::write(&target, b"old")?;

    let target = WriteTarget::resolve(&target)
        .expect("resolve target")
        .lock()?;
    assert_eq!(fs::read(target.path())?, b"old");
    assert!(matches!(
        target
            .replace(b"new", NewFileMode::Private)
            .expect("replace target"),
        CommitOutcome::Confirmed
    ));
    assert_eq!(fs::read(target.path())?, b"new");
    Ok(())
}

#[cfg(unix)]
#[test]
fn relative_symlink_alias_resolves_to_one_target_and_keeps_the_link() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let target = directory.path().join("target.txt");
    let alias = directory.path().join("alias.txt");
    fs::write(&target, b"old")?;
    symlink(Path::new("target.txt"), &alias)?;

    let resolved = WriteTarget::resolve(&alias).expect("resolve alias");
    assert_eq!(resolved.path(), fs::canonicalize(&target)?);
    resolved
        .lock()?
        .replace(b"new", NewFileMode::Private)
        .expect("replace alias target");

    assert!(fs::symlink_metadata(&alias)?.file_type().is_symlink());
    assert_eq!(fs::read(&target)?, b"new");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_cycle_is_a_typed_resolution_error() -> io::Result<()> {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    symlink(Path::new("second"), &first)?;
    symlink(Path::new("first"), &second)?;

    assert!(matches!(
        WriteTarget::resolve(&first),
        Err(ResolveTargetError::SymlinkCycle)
    ));
    Ok(())
}

/// A write leaves nothing behind next to the file it wrote.
///
/// The lock lives for the life of the machine — it can never be removed without letting two writers
/// lock different files for one target — so it must not live in the directory the user pointed
/// Bootty at. It used to, and `.hooks.json.bootty-write.lock` turned up in checked-out
/// repositories.
#[test]
fn a_write_leaves_no_lock_file_beside_its_target() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("hooks.json");

    WriteTarget::resolve(&target)
        .expect("resolve target")
        .lock()?
        .replace(b"{}", NewFileMode::UmaskWritable)
        .expect("replace target");

    let left_behind = fs::read_dir(directory.path())?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    assert_eq!(
        left_behind,
        ["hooks.json"],
        "only the file that was asked for"
    );
    Ok(())
}

/// The lock files older builds left in the user's directories are cleared by the next write to
/// that file, so nobody has to hunt them down by hand.
#[test]
fn a_write_clears_the_lock_file_an_older_build_left_behind() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("hooks.json");
    fs::write(&target, b"{}")?;
    let legacy = directory.path().join(".hooks.json.bootty-write.lock");
    fs::write(&legacy, b"")?;

    WriteTarget::resolve(&target)
        .expect("resolve target")
        .lock()?
        .replace(b"{\"a\":1}", NewFileMode::UmaskWritable)
        .expect("replace target");

    assert!(!legacy.exists(), "the stale lock file is gone");
    Ok(())
}
