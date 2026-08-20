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
