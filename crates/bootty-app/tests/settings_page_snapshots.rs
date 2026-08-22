//! Characterization of what each settings page actually renders.
//!
//! The refactor that split the settings surface into a declarative schema silently dropped rows,
//! labels and controls. Comparing every page's painted text against a committed baseline is the
//! only check that notices; a page's rendered content is the behavior under test.
//!
//! Run with `UPDATE_SETTINGS_SNAPSHOTS=1` to rewrite the baselines after an intended change.
//!
//! Limit: the surface renders with no extension host, so the Sidebar and Status pages show
//! "Loading module source…" where a real session shows the editor and its preview. Those are
//! covered by the unit tests in `ui::settings::surface::modules`, not here.

use std::path::Path;

use bootty_app::{
    theme::theme_from_config,
    ui::settings::{SettingsPage, SettingsSurface},
};
use bootty_config::config::{AppearanceVariant, BoottyConfig, load_or_create_config_document};
use bootty_extension::ModuleSources;
use bootty_ui::icons::install_icon_fonts;
use bootty_winit::direct_input::ModifierSideState;
use egui::{RawInput, Shape};

/// Every text run the surface painted, ordered top-to-bottom then left-to-right. Coordinates are
/// rounded so sub-pixel layout jitter cannot churn the baseline.
fn painted_text(page: SettingsPage) -> String {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    let theme = theme_from_config(&config, AppearanceVariant::Dark);
    let document = load_or_create_config_document(&config.config_path).expect("empty document");
    let mut settings = SettingsSurface::new(config, document);
    settings.set_page(page);

    let context = egui::Context::default();
    install_icon_fonts(&context);
    let input = || RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1200.0),
        )),
        ..RawInput::default()
    };
    // Two passes: the first frame settles lazily-loaded lists (fonts, themes, keybind rows) that
    // the surface fills in on demand, so only the second frame is representative.
    context
        .run_ui(input(), |ui| {
            settings.show(
                ui,
                theme,
                Vec::new(),
                ModifierSideState::default(),
                ModuleSources::default(),
            );
        })
        .drop_without_applying_deltas();
    let output = context.run_ui(input(), |ui| {
        settings.show(
            ui,
            theme,
            Vec::new(),
            ModifierSideState::default(),
            ModuleSources::default(),
        );
    });

    // The config directory is a fresh temp path each run; the baseline names the role, not the path.
    let root = directory.path().to_string_lossy().into_owned();
    let mut runs = Vec::new();
    for clipped in &output.shapes {
        collect_text(&clipped.shape, &mut runs);
    }
    output.drop_without_applying_deltas();
    runs.sort_by_key(|run| (run.0, run.1));
    let mut lines = runs
        .into_iter()
        .map(|(_, _, text)| text.replace(&root, "<config-dir>"))
        .collect::<Vec<_>>();
    lines.dedup();
    lines.join("\n") + "\n"
}

fn collect_text(shape: &Shape, out: &mut Vec<(i32, i32, String)>) {
    match shape {
        Shape::Text(text) => {
            let trimmed = text.galley.text().trim();
            if !trimmed.is_empty() {
                out.push((
                    text.pos.y.round() as i32,
                    text.pos.x.round() as i32,
                    trimmed.replace('\n', "\\n"),
                ));
            }
        }
        Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect_text(shape, out)),
        _ => {}
    }
}

#[test]
fn every_settings_page_renders_its_baseline_content() {
    let update = std::env::var_os("UPDATE_SETTINGS_SNAPSHOTS").is_some();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/settings");
    if update {
        std::fs::create_dir_all(&directory).expect("snapshot directory");
    }
    let mut failures = Vec::new();
    for page in SettingsSurface::pages() {
        let name = format!("{page:?}").to_ascii_lowercase();
        let path = directory.join(format!("{name}.txt"));
        let rendered = painted_text(page);
        if update {
            std::fs::write(&path, &rendered).expect("write snapshot");
            continue;
        }
        let baseline = std::fs::read_to_string(&path).unwrap_or_default();
        if baseline != rendered {
            failures.push(format!(
                "{name}: rendered content changed\n--- baseline\n{baseline}--- rendered\n{rendered}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "settings pages drifted from their baselines (re-run with \
         UPDATE_SETTINGS_SNAPSHOTS=1 once the change is intended):\n\n{}",
        failures.join("\n")
    );
}
