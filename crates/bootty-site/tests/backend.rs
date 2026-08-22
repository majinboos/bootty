use bootty_site::{
    SiteBackend,
    web_frame::{WebCell, WebTerminalFrame},
};

#[test]
fn page_routes_select_the_expected_section() {
    let overview = render_page("overview");
    let quickstart = render_page("quickstart");
    let docs = render_page("docs");
    let renderer = render_page("renderer");

    assert_eq!(overview.selected, 0);
    assert_ne!(quickstart.selected, overview.selected);
    assert_ne!(docs.selected, quickstart.selected);
    assert_ne!(renderer.selected, docs.selected);
    assert_eq!(render_page("javascript").selected, docs.selected);
    assert_eq!(render_page("rust").selected, docs.selected);
    assert_eq!(render_page("not-a-page").selected, overview.selected);
    assert_eq!(renderer.focus, "detail");
}

#[test]
fn public_mouse_tabs_follow_the_rendered_nested_tab_row() {
    let mut site = SiteBackend::for_page("docs");
    let initial = site.render_frame().expect("render docs");
    let (tab_y, row) = rows(&initial)
        .into_iter()
        .enumerate()
        .find(|(_, row)| row.contains("Browser") && row.contains("Node"))
        .expect("render Browser and Node tabs");
    let browser_x = row
        .find("Browser")
        .expect("render Browser tab")
        .saturating_add(1) as u16;
    let node_x = row.find("Node").expect("render Node tab").saturating_add(1) as u16;

    let browser = site
        .mouse_frame("down", browser_x, tab_y as u16, 0)
        .expect("click Browser tab");
    assert!(
        frame_text(&browser).contains("mountCanvasTerminal"),
        "clicking the rendered Browser tab must select the Browser content"
    );

    let selected = site
        .mouse_frame("down", node_x, tab_y as u16, 0)
        .expect("click Node tab");

    assert!(
        frame_text(&selected).contains("createEmptyFrame"),
        "clicking the rendered Node tab must select the Node content"
    );
}

#[test]
fn non_alternative_pages_keep_content_clicks_out_of_tab_dispatch() {
    for page in ["overview", "quickstart", "renderer", "config"] {
        let mut site = SiteBackend::for_page(page);
        site.mouse_frame("down", 5, 6, 0)
            .unwrap_or_else(|error| panic!("begin {page} content selection: {error}"));
        site.mouse_frame("move", 12, 6, 0)
            .unwrap_or_else(|error| panic!("drag {page} content selection: {error}"));
        site.mouse_frame("up", 12, 6, 0)
            .unwrap_or_else(|error| panic!("finish {page} content selection: {error}"));

        assert!(
            site.selected_text().is_some(),
            "{page} content click must not be consumed as a fake tab hit"
        );
    }
}

#[test]
fn mouse_selection_and_scroll_stay_in_the_host_neutral_backend() {
    let mut site = SiteBackend::for_page("quickstart");
    let before = site.render_frame().expect("render quickstart");

    let moved = site
        .mouse_frame("move", 2, 10, 0)
        .expect("handle mouse move");
    let pressed = site
        .mouse_frame("down", 2, 10, 0)
        .expect("handle mouse down");
    assert_eq!(moved.selected, before.selected);
    assert_eq!(pressed.selected, before.selected);
    assert_eq!(pressed.focus, "detail");

    let scrolled = site
        .mouse_frame("wheel", 40, 8, 3)
        .expect("handle detail scroll");
    assert_eq!(scrolled.selected, before.selected);
    assert_eq!(scrolled.focus, "detail");
    assert_ne!(rows(&scrolled), rows(&before));

    site.mouse_frame("down", 4, 3, 0).expect("begin selection");
    site.mouse_frame("move", 12, 3, 0).expect("drag selection");
    let selected = site.mouse_frame("up", 12, 3, 0).expect("finish selection");
    let selection = selected.selection.expect("selection is exported");
    assert_eq!((selection.anchor.x, selection.anchor.y), (4, 3));
    assert_eq!((selection.focus.x, selection.focus.y), (12, 3));
}

#[test]
fn frame_uses_the_full_terminal_canvas_and_exports_wire_defaults() {
    let mut site = SiteBackend::new();
    let frame = site.resize_frame(96, 32).expect("render site frame");
    let egui = frame
        .egui
        .as_ref()
        .expect("egui field remains in wire frame");

    assert_eq!((frame.cols, frame.rows), (96, 32));
    assert!(egui.meshes.is_empty());
    assert!(egui.links.is_empty());
    assert!(frame.cells.iter().any(|cell| cell.text == "T"));
    assert!(
        frame
            .cells
            .iter()
            .any(|cell| cell.fg.is_none() && cell.bg.is_none()),
        "unstyled cells must inherit frame colors"
    );
    assert!(
        frame.cells.iter().any(|cell| cell.fg.is_some()),
        "styled cells must carry explicit terminal colors"
    );
}

#[test]
fn product_pages_render_complete_content_without_markdown_fences() {
    let overview = scrolling_text(&mut SiteBackend::for_page("overview"));
    assert!(overview.contains("What ships"));
    assert!(overview.contains("Architecture map"));
    assert!(overview.contains("Documentation map"));

    let quickstart = scrolling_text(&mut SiteBackend::for_page("quickstart"));
    assert!(quickstart.contains("Native app"));
    assert!(quickstart.contains("Glyph probe"));
    assert!(quickstart.contains("cargo run -p bootty --bin bootty"));

    let renderer = scrolling_text(&mut SiteBackend::for_page("renderer"));
    assert!(renderer.contains("Frame contract"));
    assert!(renderer.contains("Font feature probes"));
    assert!(renderer.contains("Color, links, and images"));

    assert!(!overview.contains("```"));
    assert!(!quickstart.contains("```"));
    assert!(!renderer.contains("```"));
}

#[test]
fn docs_keyboard_navigation_renders_each_nested_package_surface() {
    let mut site = SiteBackend::for_page("docs");
    let npm_browser = scrolling_text(&mut site);
    assert!(npm_browser.contains("npm install bootty.js"));
    assert!(npm_browser.contains("mountCanvasTerminal"));

    site.input_frame("l").expect("switch to Node docs");
    assert!(scrolling_text(&mut site).contains("createEmptyFrame"));

    site.input_frame("]").expect("switch to Rust docs");
    let rust = scrolling_text(&mut site);
    assert!(rust.contains("TerminalSession::new_with_repaint_wakeup"));
    assert!(rust.contains("TerminalTextContract::for_terminal_paint_plan"));
    assert!(rust.contains("planner preserves terminal appearance and placement as paint commands"));
    assert!(!rust.contains("```"));
}

#[test]
fn config_examples_keep_distinct_toml_styles() {
    let frame = render_page("config");
    let (theme_x, theme_y) = find_text(&frame, "theme = \"Catppuccin Mocha\"");
    let (table_x, table_y) = find_text(&frame, "[window]");
    let theme_key = cell(&frame, theme_x, theme_y)
        .fg
        .expect("theme key has a color");
    let theme_value = cell(&frame, theme_x + 8, theme_y)
        .fg
        .expect("theme value has a color");
    let table = cell(&frame, table_x, table_y)
        .fg
        .expect("table has a color");

    assert_ne!(theme_key, theme_value);
    assert_ne!(table, theme_value);
}

#[test]
fn double_and_triple_click_select_visible_rust_text() {
    let mut site = SiteBackend::new();
    let frame = site.render_frame().expect("render overview");
    let (word_x, word_y) = find_text(&frame, "Bootty");

    site.mouse_frame("down", word_x + 2, word_y, 2)
        .expect("select visible word");
    assert_eq!(site.selected_text().as_deref(), Some("Bootty"));

    let frame = site.render_frame().expect("render selected overview");
    let (line_x, line_y) = find_text(&frame, "Bootty is a terminal product");
    let expected = rows(&frame)[line_y as usize].trim_end().to_owned();
    site.mouse_frame("down", line_x + 5, line_y, 3)
        .expect("select visible line");
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

fn scrolling_text(site: &mut SiteBackend) -> String {
    let mut text = frame_text(&site.render_frame().expect("render page"));
    for _ in 0..40 {
        let frame = site.input_frame("\u{1b}[6~").expect("page through content");
        text.push('\n');
        text.push_str(&frame_text(&frame));
    }
    text
}

fn rows(frame: &WebTerminalFrame) -> Vec<String> {
    (0..frame.rows)
        .map(|y| {
            (0..frame.cols)
                .map(|x| cell(frame, x, y).text.as_str())
                .collect::<String>()
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
