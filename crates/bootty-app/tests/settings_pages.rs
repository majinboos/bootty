use bootty_app::ui::settings::surface::SettingsSurface;

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
