use std::{fs, io, path::Path};

use assert_fs::{TempDir, prelude::*};
use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::{fixture, rstest};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Arbitrary, Debug)]
struct PathCase {
    #[proptest(regex = "[a-z][a-z0-9-]{0,31}\\.bin")]
    name: String,
}

#[fixture]
fn directory() -> TempDir {
    TempDir::new().expect("temporary directory")
}

#[cfg(unix)]
#[test]
fn relative_symlink_alias_resolves_to_one_target_and_keeps_the_link() -> TestResult {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new()?;
    let target = directory.child("target.txt");
    let alias = directory.child("alias.txt");
    target.write_binary(b"old")?;
    symlink(Path::new("target.txt"), alias.path())?;

    let resolved = WriteTarget::resolve(alias.path()).expect("resolve alias");
    assert_eq!(resolved.path(), fs::canonicalize(target.path())?);
    resolved
        .lock()?
        .replace(b"new", NewFileMode::Private)
        .expect("replace alias target");

    assert!(fs::symlink_metadata(alias.path())?.file_type().is_symlink());
    assert_eq!(fs::read(target.path())?, b"new");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_cycle_is_a_typed_resolution_error() -> TestResult {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new()?;
    let first = directory.child("first");
    let second = directory.child("second");
    symlink(Path::new("second"), first.path())?;
    symlink(Path::new("first"), second.path())?;

    assert!(matches!(
        WriteTarget::resolve(first.path()),
        Err(ResolveTargetError::SymlinkCycle)
    ));
    Ok(())
}

#[test]
fn writes_remove_legacy_locks_and_leave_no_new_lock_beside_the_target() -> TestResult {
    let directory = TempDir::new()?;
    let target = directory.child("hooks.json");
    target.write_binary(b"{}")?;
    let legacy = directory.child(".hooks.json.bootty-write.lock");
    legacy.touch()?;

    WriteTarget::resolve(target.path())
        .expect("resolve target")
        .lock()?
        .replace(b"{\"a\":1}", NewFileMode::UmaskWritable)
        .expect("replace target");

    let left_behind = fs::read_dir(directory.path())?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    assert_eq!(left_behind, ["hooks.json"]);
    Ok(())
}

proptest! {
    /// Property: lexical current-directory components never change the resolved write target.
    #[test]
    fn lexical_current_directory_components_preserve_the_target(case in any::<PathCase>()) {
        let directory = TempDir::new().expect("temporary directory");
        let canonical = fs::canonicalize(directory.path()).expect("canonical temporary directory");
        let direct = WriteTarget::resolve(&directory.path().join(&case.name))
            .expect("direct target");
        let dotted = WriteTarget::resolve(&directory.path().join(".").join(&case.name))
            .expect("target containing current-directory component");

        prop_assert_eq!(direct.path(), canonical.join(&case.name));
        prop_assert_eq!(dotted.path(), direct.path());
    }
}

#[rstest]
fn commits_exact_bytes(directory: TempDir) {
    let target = directory.child("state.bin");
    let locked = WriteTarget::resolve(target.path())
        .expect("resolve target")
        .lock()
        .expect("lock target");
    let first = b"\0Bootty\xff";
    let second = (u8::MIN..=u8::MAX).collect::<Vec<_>>();

    let outcome = locked
        .replace(first, NewFileMode::Private)
        .expect("first commit");
    assert!(matches!(outcome, CommitOutcome::Confirmed));
    assert_eq!(fs::read(target.path()).expect("read first commit"), first);

    locked
        .replace(&second, NewFileMode::Private)
        .expect("replacement commit");
    assert_eq!(
        fs::read(target.path()).expect("read replacement commit"),
        second
    );
}
