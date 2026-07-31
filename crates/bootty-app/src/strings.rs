use std::path::{Path, PathBuf};

pub fn display_path(path: &str) -> String {
    let path = Path::new(path);
    if let Some(home) = home_dir()
        && let Ok(relative) = path.strip_prefix(home)
    {
        return Path::new("~").join(relative).display().to_string();
    }
    path.display().to_string()
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
    if let Some(rest) = home_relative_path(path)
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn home_relative_path(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(rest);
    }
    #[cfg(windows)]
    {
        path.strip_prefix(r"~\")
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn home_dir() -> Option<PathBuf> {
    crate::config::default_working_directory()
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
pub fn unique_session_name<'a, I>(candidate: &str, existing: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let existing = existing
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    if !existing.contains(candidate) {
        return candidate.to_owned();
    }

    let (group, leaf) = candidate.rsplit_once('/').unwrap_or(("", candidate));
    for suffix in 2.. {
        let suffixed_leaf = format!("{leaf}-{suffix}");
        let name = if group.is_empty() {
            suffixed_leaf
        } else {
            format!("{group}/{suffixed_leaf}")
        };
        if !existing.contains(name.as_str()) {
            return name;
        }
    }
    unreachable!("session name suffix range is unbounded")
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

    #[test]
    fn unique_session_name_suffixes_collisions_inside_a_group() {
        assert_eq!(
            unique_session_name("bootty/review", ["bootty/review", "bootty/review-2"]),
            "bootty/review-3"
        );
    }

    #[test]
    fn unique_session_name_suffixes_ungrouped_collisions() {
        assert_eq!(unique_session_name("scratch", ["scratch"]), "scratch-2");
    }

    #[test]
    fn unique_session_name_keeps_available_names() {
        assert_eq!(
            unique_session_name("bootty/main", ["other/main"]),
            "bootty/main"
        );
    }

    #[test]
    fn display_path_contracts_home_without_contracting_lookalike_prefixes() {
        let Some(home) = home_dir() else {
            return;
        };
        let child = home.join("src");
        let lookalike = PathBuf::from(format!("{}-backup", home.display()));

        assert_eq!(
            display_path(&child.to_string_lossy()),
            Path::new("~").join("src").display().to_string()
        );
        assert_eq!(
            display_path(&lookalike.to_string_lossy()),
            lookalike.display().to_string()
        );
    }

    #[cfg(windows)]
    #[test]
    fn expand_home_path_accepts_windows_separator() {
        let Some(home) = home_dir() else {
            return;
        };

        assert_eq!(expand_home_path(r"~\src"), home.join("src"));
    }
}
