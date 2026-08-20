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

pub fn session_name_for_remote_path(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|name| !name.is_empty() && !name.ends_with(':'))
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

/// Whether `name` is what `unique_session_name` would produce from `base`: `base` itself, or `base`
/// with a numeric suffix on its leaf. Recognizing bootty's own uniqueness suffix is what tells a
/// backend name bootty asked for apart from a name someone else chose.
pub fn is_uniquified_session_name(name: &str, base: &str) -> bool {
    if name == base {
        return true;
    }
    let (group, leaf) = base.rsplit_once('/').unwrap_or(("", base));
    let Some(candidate_leaf) = name
        .strip_prefix(group)
        .and_then(|rest| rest.strip_prefix(if group.is_empty() { "" } else { "/" }))
    else {
        return false;
    };
    candidate_leaf
        .strip_prefix(leaf)
        .and_then(|suffix| suffix.strip_prefix('-'))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.chars().all(|char| char.is_ascii_digit())
        })
}

pub fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}
