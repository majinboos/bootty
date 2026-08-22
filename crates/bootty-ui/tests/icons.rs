use bootty_ui::icons::{has_slug, icon_glyph, resolve_slug};
use pretty_assertions::assert_eq;
#[test]
fn public_chrome_icon_slugs_resolve_to_drawable_glyphs() {
    for slug in [
        "folder",
        "coffee-cup",
        "coffee-cup-filled",
        "plug",
        "plug-zap",
        "battery-charging",
        "battery-full",
        "cpu",
        "memory-stick",
        "calendar",
        "clock",
        "openai",
        "claude",
        "anthropic",
        "bootstrap:openai",
        "phosphor:alarm",
        "command",
        "option",
        "arrow-big-up",
        "chevron-up",
        "chevron-right",
        "grip-vertical",
        "sliders-horizontal",
        "arrow-left",
        "arrow-right",
        "check",
        "circle-alert",
        "plus",
    ] {
        assert!(has_slug(slug), "missing public icon {slug}");
        assert!(
            resolve_slug(slug).is_some(),
            "unresolved public icon {slug}"
        );
        assert!(icon_glyph(slug).is_some(), "undrawable public icon {slug}");
    }
}

#[test]
fn unknown_icon_slug_is_rejected() {
    let slug = "not-a-real-lucide-icon";
    assert_eq!(
        (has_slug(slug), resolve_slug(slug), icon_glyph(slug)),
        (false, None, None)
    );
}
