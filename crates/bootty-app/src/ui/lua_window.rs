//! Renders a Luau-declared floating window ([`crate::extensions::LuaWindowSpec`])
//! through the native overlay framework. Extensions describe windows as data and
//! handle the user's choice via their `on_action` handler; they never touch egui.

use bootty_ui::Theme;
use eframe::egui;

use crate::extensions::{LuaWindowRow, LuaWindowSpec};
use crate::ui::overlay::{self, FloatingWindow, ListRow, ListView, TextPrompt, list};

#[derive(Clone, Debug)]
pub struct LuaWindowDialog {
    spec: LuaWindowSpec,
    filter: String,
    selected: usize,
    input: String,
    focus: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LuaWindowEvent {
    None,
    Close,
    /// The user picked a row (`key`) or submitted the prompt (`value`).
    Action {
        key: String,
        value: Option<String>,
    },
}

impl LuaWindowDialog {
    pub fn new(spec: LuaWindowSpec) -> Self {
        Self {
            spec,
            filter: String::new(),
            selected: 0,
            input: String::new(),
            focus: true,
        }
    }

    /// The window id, used to route the action back to the owning worker handler.
    pub fn id(&self) -> u64 {
        self.spec.id
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> LuaWindowEvent {
        if self.spec.kind == "prompt" {
            self.show_prompt(ctx, theme)
        } else {
            self.show_list(ctx, theme)
        }
    }

    fn show_list(&mut self, ctx: &egui::Context, theme: Theme) -> LuaWindowEvent {
        let matches = filtered(&self.spec.rows, &self.filter);
        self.selected = list::clamp_selection(self.selected, matches.len());
        let rows: Vec<ListRow> = matches
            .iter()
            .filter_map(|&index| self.spec.rows.get(index))
            .map(|row| ListRow {
                icon: row.icon.clone(),
                primary: row.text.clone(),
                trailing: row.description.clone(),
                ..ListRow::default()
            })
            .collect();
        let list_max = overlay::list_max_height(ctx, 150.0, 520.0);

        let result = self
            .window(ctx, "Enter select   Esc close")
            .footer(format!(
                "{} / {} items",
                matches.len(),
                self.spec.rows.len()
            ))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new(("lua-window-filter", self.spec.id)),
                    &mut self.filter,
                    theme,
                    "filter...",
                );
                if self.focus {
                    filter.request_focus();
                    self.focus = false;
                }
                ui.add_space(8.0);
                let outcome =
                    ListView::new(("lua-window-list", self.spec.id), &rows, self.selected)
                        .max_height(list_max)
                        .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(row) = matches.get(index).and_then(|&i| self.spec.rows.get(i))
        {
            return LuaWindowEvent::Action {
                key: row.key.clone(),
                value: None,
            };
        }
        self.dismissed(&result)
    }

    fn show_prompt(&mut self, ctx: &egui::Context, theme: Theme) -> LuaWindowEvent {
        let placeholder = self.spec.placeholder.clone().unwrap_or_default();
        let result =
            self.window(ctx, "Enter submit   Esc cancel")
                .show(ctx, theme, |ui, _palette| {
                    TextPrompt::new(("lua-window-prompt", self.spec.id))
                        .hint(&placeholder)
                        .submit_disabled(self.input.trim().is_empty())
                        .show(ui, theme, &mut self.input, &mut self.focus)
                });

        if result.inner.submitted && !self.input.trim().is_empty() {
            return LuaWindowEvent::Action {
                key: "submit".to_owned(),
                value: Some(self.input.trim().to_owned()),
            };
        }
        self.dismissed(&result)
    }

    fn window(&self, ctx: &egui::Context, hint: &'static str) -> FloatingWindow {
        let mut window = FloatingWindow::new(("lua-window", self.spec.id), self.spec.title.clone())
            .hint(self.spec.hint.clone().unwrap_or_else(|| hint.to_owned()))
            .width(overlay::panel_width(ctx, 720.0, 420.0));
        if let Some(icon) = &self.spec.icon {
            window = window.icon(icon.clone());
        }
        window
    }

    fn dismissed<R>(&self, result: &overlay::OverlayResult<R>) -> LuaWindowEvent {
        if result.escaped || result.clicked_outside {
            LuaWindowEvent::Close
        } else {
            LuaWindowEvent::None
        }
    }
}

fn filtered(rows: &[LuaWindowRow], filter: &str) -> Vec<usize> {
    let filter = filter.trim();
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (filter.is_empty()
                || overlay::fuzzy_match(&row.text, filter)
                || overlay::fuzzy_match(&row.key, filter))
            .then_some(index)
        })
        .collect()
}
