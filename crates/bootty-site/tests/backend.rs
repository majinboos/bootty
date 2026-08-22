use bootty_site::{
    SiteBackend,
    web_frame::{WebCell, WebTerminalFrame},
};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("overview", 0)]
#[case("quickstart", 1)]
#[case("docs", 2)]
#[case("javascript", 2)]
#[case("rust", 2)]
#[case("renderer", 3)]
fn named_routes_select_their_navigation_section(#[case] page: &str, #[case] selected: usize) {
    assert_eq!(render_page(page).selected, selected);
}

proptest! {
    /// An unrecognized route always falls back to the overview section.
    #[test]
    fn unknown_routes_fall_back_to_overview(page in "unknown-[a-z0-9-]{0,24}") {
        let frame = SiteBackend::for_page(&page)
            .resize_frame(1, 1)
            .expect("render minimal route frame");
        prop_assert_eq!(frame.selected, 0);
    }
}

#[test]
fn mouse_tabs_use_the_coordinates_of_the_rendered_nested_tab_row() {
    let mut site = SiteBackend::for_page("docs");
    let initial = site.render_frame().expect("render docs");
    let (tab_y, row) = rows(&initial)
        .into_iter()
        .enumerate()
        .find(|(_, row)| row.contains("Browser") && row.contains("Node"))
        .expect("render Browser and Node tabs");

    for (label, content) in [
        ("Browser", "mountCanvasTerminal"),
        ("Node", "createEmptyFrame"),
    ] {
        let x = row.find(label).expect("render nested tab") as u16 + 1;
        let frame = site
            .mouse_frame("down", x, tab_y as u16, 0)
            .expect("click nested tab");
        assert!(frame_text(&frame).contains(content));
    }
}

#[test]
fn mouse_selection_and_scroll_preserve_page_and_focus() {
    let mut site = SiteBackend::for_page("quickstart");
    let before = site.render_frame().expect("render quickstart");
    for kind in ["move", "down"] {
        let frame = site.mouse_frame(kind, 2, 10, 0).expect("handle pointer");
        assert_eq!((frame.selected, frame.focus), (before.selected, "detail"));
    }
    let scrolled = site.mouse_frame("wheel", 40, 8, 3).expect("scroll detail");
    assert_eq!(
        (scrolled.selected, scrolled.focus),
        (before.selected, "detail")
    );
    assert_ne!(rows(&scrolled), rows(&before));

    for (kind, x) in [("down", 4), ("move", 12)] {
        site.mouse_frame(kind, x, 3, 0).expect("drag selection");
    }
    let selection = site
        .mouse_frame("up", 12, 3, 0)
        .expect("finish selection")
        .selection
        .expect("export selection");
    assert_eq!((selection.anchor.x, selection.anchor.y), (4, 3));
    assert_eq!((selection.focus.x, selection.focus.y), (12, 3));
}

#[test]
fn frame_fills_the_requested_terminal_canvas_and_keeps_wire_defaults() {
    let frame = SiteBackend::new()
        .resize_frame(96, 32)
        .expect("render site");
    let egui = frame.egui.as_ref().expect("egui wire field");
    assert_eq!((frame.cols, frame.rows), (96, 32));
    assert!(egui.meshes.is_empty() && egui.links.is_empty());
    assert!(
        frame
            .cells
            .iter()
            .any(|cell| cell.fg.is_none() && cell.bg.is_none())
    );
    assert!(frame.cells.iter().any(|cell| cell.fg.is_some()));
}

#[rstest]
#[case("overview", "Architecture map")]
#[case("quickstart", "cargo run -p bootty --bin bootty")]
#[case("renderer", "Font feature probes")]
fn product_pages_render_their_distinct_content(#[case] page: &str, #[case] marker: &str) {
    let text = complete_page_text(&mut SiteBackend::for_page(page));
    assert!(text.contains(marker), "{page} omitted {marker:?}");
    assert!(!text.contains("```"));
}

#[test]
fn keyboard_navigation_renders_each_documentation_package() {
    let mut site = SiteBackend::for_page("docs");
    let browser = complete_page_text(&mut site);
    assert!(browser.contains("npm install bootty.js") && browser.contains("mountCanvasTerminal"));
    site.input_frame("l").expect("switch to Node docs");
    assert!(complete_page_text(&mut site).contains("createEmptyFrame"));
    site.input_frame("]").expect("switch to Rust docs");
    let rust = complete_page_text(&mut site);
    for needle in [
        "TerminalSession::new_with_repaint_wakeup",
        "TerminalTextContract::for_terminal_paint_plan",
        "planner preserves terminal appearance and placement as paint commands",
    ] {
        assert!(rust.contains(needle));
    }
    assert!(!rust.contains("```"));
}

#[test]
fn toml_tokens_use_distinct_styles() {
    let frame = render_page("config");
    let (theme_x, theme_y) = find_text(&frame, "theme = \"Catppuccin Mocha\"");
    let (table_x, table_y) = find_text(&frame, "[window]");
    let theme_key = cell(&frame, theme_x, theme_y).fg.expect("theme key color");
    let theme_value = cell(&frame, theme_x + 8, theme_y)
        .fg
        .expect("theme value color");
    let table = cell(&frame, table_x, table_y).fg.expect("table color");
    assert_ne!(theme_key, theme_value);
    assert_ne!(table, theme_value);
}

#[rstest]
#[case(2, "Bootty")]
#[case(3, "Bootty is a terminal product")]
fn repeated_click_selects_the_visible_text_unit(#[case] clicks: i16, #[case] needle: &str) {
    let mut site = SiteBackend::new();
    let frame = site.render_frame().expect("render overview");
    let (x, y) = find_text(&frame, needle);
    let expected = if clicks == 2 {
        "Bootty".to_owned()
    } else {
        rows(&frame)[usize::from(y)].trim_end().to_owned()
    };
    site.mouse_frame("down", x + 2, y, clicks)
        .expect("select text unit");
    assert_eq!(site.selected_text().as_deref(), Some(expected.as_str()));
}

fn render_page(page: &str) -> WebTerminalFrame {
    SiteBackend::for_page(page)
        .render_frame()
        .unwrap_or_else(|error| panic!("render {page}: {error}"))
}

fn frame_text(frame: &WebTerminalFrame) -> String {
    rows(frame).join("\n")
}

fn complete_page_text(site: &mut SiteBackend) -> String {
    frame_text(
        &site
            .resize_frame(120, 200)
            .expect("render complete page in a tall viewport"),
    )
}

fn rows(frame: &WebTerminalFrame) -> Vec<String> {
    (0..frame.rows)
        .map(|y| {
            (0..frame.cols)
                .map(|x| cell(frame, x, y).text.as_str())
                .collect()
        })
        .collect()
}

fn find_text(frame: &WebTerminalFrame, needle: &str) -> (u16, u16) {
    rows(frame)
        .into_iter()
        .enumerate()
        .find_map(|(y, row)| row.find(needle).map(|x| (x as u16, y as u16)))
        .unwrap_or_else(|| panic!("frame does not contain {needle:?}"))
}

fn cell(frame: &WebTerminalFrame, x: u16, y: u16) -> &WebCell {
    &frame.cells[usize::from(y) * usize::from(frame.cols) + usize::from(x)]
}
