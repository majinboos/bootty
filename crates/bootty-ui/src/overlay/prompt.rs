//! A single-line text prompt for floating windows (rename, branch name, ...).
//!
//! The caller owns the text buffer and a one-shot focus flag, the same way the
//! pickers own their filter state; [`TextPrompt::show`] reports whether Enter
//! submitted this frame. Cancellation is the [`super::FloatingWindow`]'s concern.

use crate::Theme;
use eframe::egui;

/// A single-line text entry with an optional caption and inline validation.
pub struct TextPrompt<'a> {
    id: egui::Id,
    caption: Option<&'a str>,
    hint: &'a str,
    validation: Option<&'a str>,
    submit_disabled: bool,
}

/// What a [`TextPrompt`] produced for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptOutcome {
    /// Enter was pressed on the focused field with submit enabled.
    pub submitted: bool,
}

impl<'a> TextPrompt<'a> {
    pub fn new(id_source: impl std::hash::Hash + std::fmt::Debug) -> Self {
        Self {
            id: egui::Id::new(id_source),
            caption: None,
            hint: "",
            validation: None,
            submit_disabled: false,
        }
    }

    /// Small caption drawn above the field.
    #[must_use]
    pub fn caption(mut self, caption: &'a str) -> Self {
        self.caption = Some(caption);
        self
    }

    /// Placeholder shown while the field is empty.
    #[must_use]
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    /// Inline message drawn under the field (e.g. why submit is blocked).
    #[must_use]
    pub fn validation(mut self, validation: Option<&'a str>) -> Self {
        self.validation = validation;
        self
    }

    /// Suppress Enter submission (e.g. while the input is invalid).
    #[must_use]
    pub fn submit_disabled(mut self, disabled: bool) -> Self {
        self.submit_disabled = disabled;
        self
    }

    /// Render the prompt. `buf` holds the text; set `*focus` to grab focus on the
    /// first frame (it is cleared afterwards).
    pub fn show(
        self,
        ui: &mut egui::Ui,
        theme: Theme,
        buf: &mut String,
        focus: &mut bool,
    ) -> PromptOutcome {
        let palette = theme.palette;
        if let Some(caption) = self.caption {
            ui.label(
                egui::RichText::new(caption)
                    .monospace()
                    .size(12.0)
                    .color(palette.muted),
            );
            ui.add_space(4.0);
        }
        let response = crate::themed_text_edit_singleline(ui, buf, theme, |edit| {
            edit.id(self.id)
                .desired_width(f32::INFINITY)
                .hint_text(self.hint)
        });
        if *focus {
            response.request_focus();
            *focus = false;
        }
        if let Some(validation) = self.validation {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(validation)
                    .monospace()
                    .size(12.0)
                    .color(palette.destructive),
            );
        }
        let submitted = !self.submit_disabled
            && response.lost_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        PromptOutcome { submitted }
    }
}
