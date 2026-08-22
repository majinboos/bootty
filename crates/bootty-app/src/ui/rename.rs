use bootty_ui::{Theme, overlay};
use eframe::egui;

use bootty_ui::overlay::{FloatingWindow, TextPrompt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameSessionDialog {
    session_id: String,
    name: String,
    focus: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameSessionEvent {
    Close,
    Rename { session_id: String, name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameTabDialog {
    session_id: String,
    window_id: String,
    name: String,
    focus: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenameTabEvent {
    Close,
    Rename {
        session_id: String,
        window_id: String,
        name: String,
    },
}

impl RenameSessionDialog {
    pub fn open(session_id: String, current_name: String) -> Self {
        Self {
            session_id,
            name: current_name,
            focus: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> Option<RenameSessionEvent> {
        let normalized = normalized_name(&self.name);
        let validation = normalized.is_none().then_some("name cannot be empty");

        let result = FloatingWindow::new("rename-session-dialog", "Rename Session")
            .icon("square-pen")
            .hint("Enter rename   Esc close")
            .width(overlay::panel_width(ctx, 520.0, 360.0))
            .show(ctx, theme, |ui, _palette| {
                TextPrompt::new("rename-session-field")
                    .caption("session name")
                    .hint("new session name...")
                    .validation(validation)
                    .submit_disabled(normalized.is_none())
                    .show(ui, theme, &mut self.name, &mut self.focus)
            });

        if result.inner.submitted
            && let Some(name) = normalized
        {
            return Some(RenameSessionEvent::Rename {
                session_id: self.session_id.clone(),
                name,
            });
        }
        if result.escaped || result.clicked_outside {
            return Some(RenameSessionEvent::Close);
        }
        None
    }
}

impl RenameTabDialog {
    pub fn open(session_id: String, window_id: String, current_name: String) -> Self {
        Self {
            session_id,
            window_id,
            name: current_name,
            focus: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> Option<RenameTabEvent> {
        let normalized = normalized_tab_name(&self.name);

        let result = FloatingWindow::new("rename-tab-dialog", "Rename Tab")
            .icon("square-pen")
            .hint("Enter rename   Esc close")
            .width(overlay::panel_width(ctx, 520.0, 360.0))
            .footer("Clear the field to follow terminal title codes again")
            .show(ctx, theme, |ui, _palette| {
                TextPrompt::new("rename-tab-field")
                    .caption("tab name")
                    .hint("new tab name...")
                    .show(ui, theme, &mut self.name, &mut self.focus)
            });

        if result.inner.submitted {
            return Some(RenameTabEvent::Rename {
                session_id: self.session_id.clone(),
                window_id: self.window_id.clone(),
                name: normalized,
            });
        }
        if result.escaped || result.clicked_outside {
            return Some(RenameTabEvent::Close);
        }
        None
    }
}

/// Trim the raw input; reject empty/whitespace-only names so we never rename a
/// session to a blank label.
fn normalized_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn normalized_tab_name(raw: &str) -> String {
    raw.trim().to_owned()
}
