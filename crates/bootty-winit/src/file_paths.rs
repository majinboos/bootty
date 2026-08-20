use std::path::Path;

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
