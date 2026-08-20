use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSString, NSURL};

/// Read file URLs from the native pasteboard.
pub fn read_clipboard_file_paths() -> Option<Vec<PathBuf>> {
    platform_read_clipboard_file_paths()
}

#[cfg(target_os = "macos")]
fn platform_read_clipboard_file_paths() -> Option<Vec<PathBuf>> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let items = pasteboard.pasteboardItems()?;
    let mut paths = Vec::new();
    for index in 0..items.count() {
        let item = items.objectAtIndex(index);
        if let Some(url) = item.stringForType(unsafe { NSPasteboardTypeFileURL })
            && let Some(path) = path_from_file_url(&url.to_string())
        {
            paths.push(path);
        }
    }
    if paths.is_empty() { None } else { Some(paths) }
}

#[cfg(target_os = "macos")]
fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let url = NSURL::URLWithString(&NSString::from_str(url))?;
    if !url.isFileURL() {
        return None;
    }
    url.filePathURL()?.to_file_path()
}

#[cfg(not(target_os = "macos"))]
fn platform_read_clipboard_file_paths() -> Option<Vec<PathBuf>> {
    None
}

pub fn format_file_paths_for_paste<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Option<String> {
    let formatted = paths.into_iter().map(shell_quote_path).collect::<Vec<_>>();
    if formatted.is_empty() {
        None
    } else {
        Some(formatted.join(" "))
    }
}

fn shell_quote_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path.chars().all(is_unquoted_shell_path_char) {
        return path.into_owned();
    }

    format!("'{}'", path.replace('\'', "'\\''"))
}

fn is_unquoted_shell_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '+' | '=' | ':' | ',')
}
