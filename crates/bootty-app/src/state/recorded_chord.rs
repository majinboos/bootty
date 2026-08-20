/// Lowercase a single-letter key in a recorded chord (the physical-key serializer emits uppercase
/// letters, but bootty's default keybinds and the egui-path recorder use lowercase, e.g. `cmd+x`).
/// Multi-character key names like `Tab`/`F5` and non-letters are left untouched.
pub(super) fn normalize_recorded_chord(chord: String) -> String {
    match chord.rsplit_once('+') {
        Some((mods, key)) => match normalize_recorded_key(key) {
            Some(key) => format!("{mods}+{key}"),
            None => chord,
        },
        None => normalize_recorded_key(&chord).unwrap_or(chord),
    }
}

fn normalize_recorded_key(key: &str) -> Option<String> {
    if is_single_ascii_letter(key) {
        return Some(key.to_ascii_lowercase());
    }
    if let Some(letter) = key.strip_prefix("Key")
        && is_single_ascii_letter(letter)
    {
        return Some(letter.to_ascii_lowercase());
    }
    if let Some(digit) = key.strip_prefix("Digit")
        && digit.len() == 1
        && digit.as_bytes()[0].is_ascii_digit()
    {
        return Some(digit.to_owned());
    }
    None
}

fn is_single_ascii_letter(value: &str) -> bool {
    let mut chars = value.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphabetic())
}
