use std::fs;

use super::*;

#[cfg(unix)]
#[test]
fn pre_replacement_failures_keep_original_bytes_mode_and_no_temporary_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let original = "# original\n[window]\ntitle = \"old\"\n";
    fs::write(&path, original).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    let error = update_config_document_with_fault(
        &path,
        |document| document.set_item(&["window", "title"], toml_edit::value("new")),
        WriteFault::Replace,
    )
    .unwrap_err();

    assert!(error.to_string().contains(": replace:"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    let temporary_count = fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".bootty-config-")
        })
        .count();
    assert_eq!(temporary_count, 0);
}

#[test]
fn sync_failure_keeps_original_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, "[window]\ntitle = \"old\"\n").unwrap();

    let error = update_config_document_with_fault(
        &path,
        |document| document.set_item(&["window", "title"], toml_edit::value("new")),
        WriteFault::Sync,
    )
    .unwrap_err();

    assert!(error.to_string().contains(": sync:"));
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("title = \"old\"")
    );
}

#[cfg(unix)]
#[test]
fn directory_sync_failure_reports_a_committed_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, "[window]\ntitle = \"old\"\n").unwrap();

    let outcome = update_config_document_with_fault(
        &path,
        |document| document.set_item(&["window", "title"], toml_edit::value("new")),
        WriteFault::SyncDirectory,
    )
    .unwrap();

    assert!(outcome.durability_warning().is_some());
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("title = \"new\"")
    );
}
