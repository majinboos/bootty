use bootty_app::ui::settings::surface::modules::{
    SESSIONS_MODULE, displayed_source, module_preview, new_module_identity, saved_source,
};
use bootty_extension::{ModuleIdentity, preview_builtin_surfaces, preview_module_surfaces};
use eframe::egui;

#[test]
fn editor_hides_only_the_file_terminating_newline() {
    assert_eq!(displayed_source("return {}\n"), "return {}");
    assert_eq!(displayed_source("return {}\n\n"), "return {}\n");
    assert_eq!(saved_source("return {}"), "return {}\n");
    assert_eq!(saved_source("return {}\n"), "return {}\n");
}

#[test]
fn a_new_module_name_gains_the_module_extension() {
    assert_eq!(
        new_module_identity(" my_module ").map(|id| id.as_str().to_owned()),
        Ok("my_module.luau".to_owned())
    );
    assert_eq!(
        new_module_identity("nested/thing.luau").map(|id| id.as_str().to_owned()),
        Ok("nested/thing.luau".to_owned())
    );
    assert!(new_module_identity("bad name!").is_err());
}

/// Every text run the preview painted, so a regression back to labelling items — which prints
/// an icon slug where the icon belongs — fails here.
fn preview_text(source: &str) -> Vec<String> {
    let identity = ModuleIdentity::parse("preview.luau").expect("identity");
    let surfaces =
        preview_module_surfaces(&identity, source, Vec::new()).expect("preview surfaces");
    let sessions = preview_builtin_surfaces(SESSIONS_MODULE, Vec::new()).expect("session rows");
    let context = egui::Context::default();
    bootty_ui::icons::install_icon_fonts(&context);
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            module_preview(
                ui,
                bootty_ui::ThemePalette::default(),
                &surfaces,
                None,
                &sessions,
                false,
            )
        },
    );
    let mut runs = Vec::new();
    collect_text(
        &egui::Shape::Vec(
            output
                .shapes
                .iter()
                .map(|shape| shape.shape.clone())
                .collect(),
        ),
        &mut runs,
    );
    output.drop_without_applying_deltas();
    runs
}

fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(text) => {
            let text = text.galley.text().trim();
            if !text.is_empty() {
                out.push(text.to_owned());
            }
        }
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect_text(shape, out)),
        _ => {}
    }
}

/// A session module decorates the built-in session rows, so its preview has to show them —
/// on its own it renders detail rows with nothing to attach to, i.e. "no sessions".
#[test]
fn a_session_surface_previews_over_the_builtin_session_rows() {
    let runs = preview_text(
        "bootty.ui.register({ id = \"preview\", placement = \"session\" }, function()\n\
         \treturn bootty.ui.session_components({\n\
         \t\tsessions = bootty.sessions(),\n\
         \t\trender = function()\n\
         \t\t\treturn { summary = { { text = \"+7\" } } }\n\
         \t\tend,\n\
         \t})\n\
         end)\n",
    );
    assert!(
        !runs.iter().any(|run| run == "no sessions"),
        "the built-in session rows are composed in: {runs:?}"
    );
    assert!(
        runs.iter().any(|run| run.contains("api")),
        "an example session is named: {runs:?}"
    );
}

/// A sidebar module renders beside the session rows, not instead of them, so its preview shows
/// them too — and previewing `sessions` itself must not double them.
#[test]
fn a_sidebar_surface_previews_beside_the_builtin_session_rows() {
    let runs = preview_text(
        "bootty.ui.register({ id = \"preview\", placement = \"sidebar\" }, function()\n\
         \treturn { { kind = \"footer\", text = \"usage 42%\" } }\n\
         end)\n",
    );
    assert!(
        !runs.iter().any(|run| run == "no sessions"),
        "the session rows are drawn alongside: {runs:?}"
    );
    assert!(
        runs.iter().any(|run| run.contains("api")),
        "an example session is named: {runs:?}"
    );
    assert!(
        runs.iter().any(|run| run.contains("usage 42%")),
        "the module's own item is drawn: {runs:?}"
    );
}

#[test]
fn a_status_surface_previews_through_the_real_strip() {
    let runs = preview_text(
        "bootty.ui.register({ id = \"preview\", placement = \"status\" }, function()\n\
         \treturn { { text = \"42%\", icon = \"battery-charging\" } }\n\
         end)\n",
    );

    assert!(runs.iter().any(|run| run == "42%"), "{runs:?}");
    // The icon is drawn as a glyph, never spelled out.
    assert!(
        !runs.iter().any(|run| run == "battery-charging"),
        "{runs:?}"
    );
}

#[test]
fn a_sidebar_surface_previews_through_the_real_sidebar() {
    let runs = preview_text(
        "bootty.ui.register({ id = \"preview\", placement = \"sidebar\" }, function()\n\
         \treturn { { text = \"work/api\", kind = \"session\", session_id = \"$1\" } }\n\
         end)\n",
    );

    assert!(runs.iter().any(|run| run == "work/api"), "{runs:?}");
    // A session row means the mock counted it, so the empty-state text stays away.
    assert!(!runs.iter().any(|run| run == "no sessions"), "{runs:?}");
}
