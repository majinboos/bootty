use bootty_ui::{ThemePalette, keycaps::trigger_galley};
use eframe::egui::{self, Color32, RawInput};

#[test]
fn public_keycap_layout_normalizes_named_and_single_character_keys() {
    let context = egui::Context::default();
    let mut labels = Vec::new();
    let palette = ThemePalette::default();

    context
        .run_ui(RawInput::default(), |ui| {
            for trigger in ["p", "space", "escape", "esc", "enter"] {
                let galley = trigger_galley(ui, palette, trigger, Color32::WHITE, 320.0);
                labels.push(galley.job.text.clone());
            }
        })
        .drop_without_applying_deltas();

    assert_eq!(labels, ["P", "Space", "Esc", "Esc", "Enter"]);
}
