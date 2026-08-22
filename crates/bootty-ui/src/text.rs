//! Label shortening that keeps a name readable: prefer a natural break, else clip with an ellipsis.

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
