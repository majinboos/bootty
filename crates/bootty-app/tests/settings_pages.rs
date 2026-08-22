use bootty_app::ui::settings::surface::SettingsSurface;
use bootty_app::ui::settings::surface::keybinds::{action_title, humanize_action};
use bootty_config::settings_schema::SettingsSchema;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case::catalog_title("reload_config", "Reload Config")]
#[case::short_catalog_title("paste_from_clipboard", "Paste")]
#[case::parameter("decrease_font_size:1", "Decrease Font Size: 1")]
#[case::choice_title("change_appearance:dark", "Use Dark Appearance")]
fn action_titles_prefer_catalog_titles_and_keep_params(
    #[case] action: &str,
    #[case] expected: &str,
) {
    assert_eq!(action_title(action), expected);
}

#[test]
fn humanize_action_sentence_cases_names_off_the_catalog() {
    assert_eq!(humanize_action("focus_terminal"), "Focus terminal");
}

/// A nav row draws its icon from a slug, and an unresolved slug paints nothing at all -- which is
/// how the nav quietly lost every icon once before.
#[test]
fn every_settings_page_names_a_drawable_icon() {
    for page in SettingsSurface::pages() {
        let icon = SettingsSurface::page_icon(page);
        assert!(
            bootty_ui::icons::has_slug(icon),
            "the {page:?} page's icon slug `{icon}` does not resolve"
        );
    }
}

#[test]
fn every_page_is_reachable_exactly_once_from_the_nav() {
    let pages = SettingsSurface::pages().collect::<Vec<_>>();
    let mut labels = pages
        .iter()
        .map(|page| format!("{page:?}"))
        .collect::<Vec<_>>();
    labels.sort();
    let listed = labels.len();
    labels.dedup();
    assert_eq!(labels.len(), listed, "a page is listed twice in the nav");
    assert!(!pages.is_empty());
}

#[test]
fn every_registered_setting_has_a_visible_settings_page() {
    let pages = SettingsSurface::pages()
        .map(|page| page.id())
        .collect::<Vec<_>>();
    for spec in SettingsSchema::builtin().specs() {
        assert!(
            pages.contains(&spec.page.as_ref()),
            "{} points at settings page {:?}, but the page is not in the surface",
            spec.id(),
            spec.page
        );
    }
}
