/// Parse a configured `chord=action` binding for display in app dialogs.
#[must_use]
pub(super) fn parse_keybind(raw: &str) -> Option<(String, String)> {
    let (mut chord, action) = raw.rsplit_once('=')?;
    chord = chord.trim();
    while let Some(("all" | "global" | "unconsumed" | "performable", rest)) = chord.split_once(':')
    {
        chord = rest;
    }
    let action = action.trim();
    (!chord.is_empty() && !action.is_empty()).then(|| (chord.to_owned(), action.to_owned()))
}
