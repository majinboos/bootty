//! Icon rendering backed by iconflow's embedded fonts.

use std::borrow::Cow;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Pos2, RichText};
use iconflow::{Pack, Size, Style, try_icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedIcon {
    pub family: &'static str,
    pub codepoint: u32,
}

/// Resolve an icon slug exposed to status/extensions.
pub fn resolve_slug(slug: &str) -> Option<ResolvedIcon> {
    let (pack, style, slug) = icon_pack_style_and_slug(slug)?;
    let icon = try_icon(pack, slug.as_ref(), style, Size::Regular).ok()?;
    Some(ResolvedIcon {
        family: icon.family,
        codepoint: icon.codepoint,
    })
}

/// Whether a status icon slug is drawable, so layout can reserve width only when needed.
pub fn has_slug(slug: &str) -> bool {
    resolve_slug(slug).is_some()
}

/// Merge iconflow's embedded icon fonts into egui font definitions.
pub fn add_icon_fonts(fonts: &mut FontDefinitions) {
    for asset in iconflow::fonts() {
        fonts.font_data.insert(
            asset.family.to_owned(),
            std::sync::Arc::new(FontData::from_static(asset.bytes)),
        );
        fonts
            .families
            .entry(FontFamily::Name(asset.family.into()))
            .or_default()
            .push(asset.family.to_owned());
    }
}

/// Install iconflow fonts during app startup, before any paint pass asks egui to resolve them.
pub fn install_icon_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    add_icon_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

/// Paint an icon named by `slug` (as exposed to extensions), tinted.
/// Returns whether the slug resolved, so callers can lay out around it.
pub fn paint_icon_slug(
    painter: &egui::Painter,
    slug: &str,
    center: Pos2,
    size: f32,
    tint: egui::Color32,
) -> bool {
    let Some(icon) = resolve_slug(slug) else {
        return false;
    };
    let glyph = char::from_u32(icon.codepoint).unwrap_or('?');
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::new(size, FontFamily::Name(icon.family.into())),
        tint,
    );
    true
}

/// Build egui `RichText` that renders icon `slug` in its icon font at `size`,
/// tinted. Lets layouts place icons inline with native labels (the painter-based
/// `paint_icon_slug` stays for hand-laid rows). Returns `None` for unknown slugs.
pub fn icon_text(slug: &str, size: f32, tint: Color32) -> Option<RichText> {
    let icon = resolve_slug(slug)?;
    let glyph = char::from_u32(icon.codepoint)?;
    Some(
        RichText::new(glyph)
            .font(FontId::new(size, FontFamily::Name(icon.family.into())))
            .color(tint),
    )
}

/// The glyph char and its icon font family for `slug`, for callers building their own
/// `LayoutJob` sections that mix icon glyphs with text (e.g. modifier keycaps). `None` for
/// unknown slugs.
pub fn icon_glyph(slug: &str) -> Option<(char, &'static str)> {
    let icon = resolve_slug(slug)?;
    Some((char::from_u32(icon.codepoint)?, icon.family))
}

fn icon_pack_style_and_slug(slug: &str) -> Option<(Pack, Style, Cow<'_, str>)> {
    if let Some((pack, slug)) = slug.split_once(':') {
        let (pack, style, slug) = match pack {
            "bootstrap" => (Pack::Bootstrap, Style::Regular, Cow::Borrowed(slug)),
            "lucide" => (Pack::Lucide, Style::Regular, Cow::Borrowed(slug)),
            "phosphor" => (
                Pack::Phosphor,
                Style::Duotone,
                Cow::Owned(format!(
                    "{}-duotone",
                    slug.strip_suffix("-duotone").unwrap_or(slug)
                )),
            ),
            "tabler" => (Pack::Tabler, Style::Regular, Cow::Borrowed(slug)),
            _ => return None,
        };
        return Some((pack, style, slug));
    }
    let (pack, slug) = compatibility_icon(slug);
    Some((pack, Style::Regular, Cow::Borrowed(slug)))
}

fn compatibility_icon(slug: &str) -> (Pack, &str) {
    match slug {
        "coffee-cup" => (Pack::Tabler, "coffee-off"),
        "coffee-cup-filled" => (Pack::Tabler, "coffee"),
        "openai" | "claude" | "anthropic" => (Pack::Bootstrap, slug),
        other => (Pack::Lucide, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_icon_slugs_resolve_for_public_status_api() {
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
        ] {
            assert!(has_slug(slug), "missing status icon '{slug}' in iconflow");
        }
    }

    #[test]
    fn provider_logo_slugs_resolve_from_bootstrap_pack() {
        for slug in ["openai", "claude", "anthropic", "bootstrap:openai"] {
            assert!(has_slug(slug), "missing provider logo '{slug}' in iconflow");
        }
    }

    #[test]
    fn phosphor_slugs_resolve_with_the_duotone_font_variant() {
        let resolved = resolve_slug("phosphor:alarm").expect("Phosphor alarm");
        let duotone = try_icon(
            Pack::Phosphor,
            "alarm-duotone",
            Style::Duotone,
            Size::Regular,
        )
        .expect("Phosphor Duotone alarm");

        assert_eq!(resolved.family, duotone.family);
        assert_eq!(resolved.codepoint, duotone.codepoint);
    }

    #[test]
    fn missing_icon_slug_does_not_resolve() {
        assert!(!has_slug("not-a-real-lucide-icon"));
    }

    #[test]
    fn keybind_chrome_icon_slugs_resolve() {
        // Modifier keycaps draw these glyphs from the icon font because the UI font can miss
        // command/option/control glyphs; settings controls use the same path for icon-only status.
        for slug in [
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
            let glyph = icon_glyph(slug);
            assert!(glyph.is_some(), "missing keybind icon '{slug}' in iconflow");
        }
    }
}
