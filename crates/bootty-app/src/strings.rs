use std::path::{Path, PathBuf};

pub fn display_path(path: &str) -> String {
    if let Some(home) = home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}

pub fn session_name_for_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bootty")
        .trim_end_matches(".git")
        .to_owned()
}

pub fn expand_home_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn is_hidden_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != ".config")
}

pub fn truncate_label(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    push_truncated_label(&mut out, text, max_chars);
    out
}

pub fn push_truncated_label(out: &mut String, text: &str, max_chars: usize) {
    if max_chars == 0 {
        return;
    }

    let mut truncate_at = None;
    for (count, (index, _)) in text.char_indices().enumerate() {
        if count == max_chars - 1 {
            truncate_at = Some(index);
        } else if count == max_chars {
            out.push_str(&text[..truncate_at.unwrap_or(index)]);
            out.push('…');
            return;
        }
    }

    out.push_str(text);
}

pub fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_field_leaves_simple_values_unquoted() {
        assert_eq!(csv_field("arc/dblclick"), "arc/dblclick");
    }

    #[test]
    fn csv_field_quotes_values_with_commas_quotes_or_newlines() {
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn push_truncated_label_appends_exact_fit_without_ellipsis() {
        let mut out = String::from("prefix ");

        push_truncated_label(&mut out, "abcd", 4);

        assert_eq!(out, "prefix abcd");
    }

    #[test]
    fn push_truncated_label_appends_truncated_text() {
        let mut out = String::new();

        push_truncated_label(&mut out, "abcde", 4);

        assert_eq!(out, "abc…");
    }

    #[test]
    fn push_truncated_label_handles_zero_width() {
        let mut out = String::from("prefix");

        push_truncated_label(&mut out, "abc", 0);

        assert_eq!(out, "prefix");
    }
}
