use bootty_app::ui::settings::surface::modules::{
    SESSIONS_MODULE, displayed_source, module_preview, new_module_identity, saved_source,
};
use bootty_extension::{ModuleIdentity, preview_builtin_surfaces, preview_module_surfaces};
use eframe::egui;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[test]
fn editor_hides_and_restores_one_file_terminating_newline() {
    assert_eq!(displayed_source("return {}\n"), "return {}");
    assert_eq!(displayed_source("return {}\n\n"), "return {}\n");
    assert_eq!(saved_source("return {}"), "return {}\n");
    assert_eq!(saved_source("return {}\n"), "return {}\n");
}

#[rstest]
#[case::trimmed(" my_module ", Some("my_module.luau"))]
#[case::existing_extension("nested/thing.luau", Some("nested/thing.luau"))]
#[case::invalid("bad name!", None)]
fn a_new_module_name_gains_the_module_extension(
    #[case] input: &str,
    #[case] expected: Option<&str>,
) {
    assert_eq!(
        new_module_identity(input)
            .ok()
            .as_ref()
            .map(ModuleIdentity::as_str),
        expected
    );
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

#[rstest]
#[case(
        "bootty.ui.register({ id = \"preview\", placement = \"session\" }, function()\n\
         \treturn bootty.ui.session_components({\n\
         \t\tsessions = bootty.sessions(),\n\
         \t\trender = function()\n\
         \t\t\treturn { summary = { { text = \"+7\" } } }\n\
         \t\tend,\n\
         \t})\n\
         end)\n",
        &["api"],
        &["no sessions"],
)]
#[case(
        "bootty.ui.register({ id = \"preview\", placement = \"sidebar\" }, function()\n\
         \treturn { { kind = \"footer\", text = \"usage 42%\" } }\n\
         end)\n",
        &["api", "usage 42%"],
        &["no sessions"],
)]
#[case(
        "bootty.ui.register({ id = \"preview\", placement = \"status\" }, function()\n\
         \treturn { { text = \"42%\", icon = \"battery-charging\" } }\n\
         end)\n",
        &["42%"],
        &["battery-charging"],
)]
#[case(
        "bootty.ui.register({ id = \"preview\", placement = \"sidebar\" }, function()\n\
         \treturn { { text = \"work/api\", kind = \"session\", session_id = \"$1\" } }\n\
         end)\n",
        &["work/api"],
        &["no sessions"],
)]
fn previews_render_their_real_composition(
    #[case] source: &str,
    #[case] present: &[&str],
    #[case] absent: &[&str],
) {
    let runs = preview_text(source);
    for expected in present {
        assert!(runs.iter().any(|run| run.contains(expected)), "{runs:?}");
    }
    for unexpected in absent {
        assert!(!runs.iter().any(|run| run == unexpected), "{runs:?}");
    }
}
