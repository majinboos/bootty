//! A searchable palette of app commands, opened with `command_palette` (default
//! `cmd+p`). Commands and their titles/descriptions come from the shared
//! [`crate::action_catalog`]; a choice dispatches through the same path as a
//! keybinding (see `app_actions::keybind_action_for_name`).

use std::collections::HashMap;

use bootty_ui::{Theme, overlay};
use eframe::egui;

use crate::{
    action_catalog::Command, commands::CommandRegistry, ui::keybind_source::parse_keybind,
};
use bootty_ui::overlay::{FloatingWindow, ListRow, ListView};

#[derive(Clone, Debug)]
pub struct CommandPaletteDialog {
    filter: String,
    selected: usize,
    focus_filter: bool,
    /// The palette subset of the catalog, in display order.
    commands: Vec<Command>,
    /// dispatch-action string -> the chord it is bound to, for the trailing hint.
    bindings: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPaletteEvent {
    Close,
    Run(Command),
}

impl CommandPaletteDialog {
    /// `keybinds` is the active `chord=action` list, used to annotate each command
    /// with the key that triggers it.
    pub fn open(keybinds: &[String]) -> Self {
        let mut bindings = HashMap::new();
        for raw in keybinds {
            if let Some((chord, action)) = parse_keybind(raw) {
                bindings.entry(action).or_insert(chord);
            }
        }
        Self {
            filter: String::new(),
            selected: 0,
            focus_filter: true,
            commands: CommandRegistry::core().palette_commands().collect(),
            bindings,
        }
    }

    /// The base action name of the row under the cursor, for "configure this
    /// command's keybinding" (`cmd+shift+,`).
    pub fn current_action(&self) -> Option<&'static str> {
        let matches = filtered(&self.commands, &self.filter);
        matches
            .get(self.selected)
            .and_then(|matched| self.commands.get(matched.index))
            .map(|command| command.action())
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> Option<CommandPaletteEvent> {
        let matches = filtered(&self.commands, &self.filter);
        self.selected = overlay::clamp_selection(self.selected, matches.len());
        let rows: Vec<ListRow> = matches
            .iter()
            .filter_map(|matched| {
                self.commands
                    .get(matched.index)
                    .map(|command| (matched, command))
            })
            .map(|(matched, command)| {
                let trailing = command
                    .palette_action()
                    .and_then(|action| self.bindings.get(action).cloned());
                ListRow {
                    icon: Some(command.icon().to_owned()),
                    primary: command.title().to_owned(),
                    primary_matches: matched.title_indices.clone(),
                    secondary: Some(command.description().to_owned()),
                    secondary_matches: matched.description_indices.clone(),
                    trailing_matches: matched.action_indices.clone(),
                    trailing_keybind: trailing,
                    ..ListRow::default()
                }
            })
            .collect();
        let list_max = overlay::list_max_height(ctx, 220.0, 560.0);

        let result = FloatingWindow::new("command-palette-dialog", "Commands")
            .icon("search")
            .hint("Enter run   Esc close")
            .footer(format!(
                "{} / {} commands",
                matches.len(),
                self.commands.len()
            ))
            .width(overlay::panel_width(ctx, 760.0, 480.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("command-palette-filter"),
                    &mut self.filter,
                    theme,
                    "search commands...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(8.0);
                let outcome = ListView::new("command-palette-list", &rows, self.selected)
                    .max_height(list_max)
                    .row_height(44.0)
                    .empty_text("no matching commands")
                    .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(command) = matches
                .get(index)
                .and_then(|matched| self.commands.get(matched.index))
        {
            return Some(CommandPaletteEvent::Run(*command));
        }
        if result.escaped || result.clicked_outside {
            return Some(CommandPaletteEvent::Close);
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchedCommand {
    index: usize,
    score: i32,
    title_indices: Vec<usize>,
    description_indices: Vec<usize>,
    action_indices: Vec<usize>,
}

/// Commands matching `filter` (fuzzy over title, action, description), best-ranked first.
fn filtered(commands: &[Command], filter: &str) -> Vec<MatchedCommand> {
    let filter = filter.trim();
    let mut matches = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match_command(index, *command, filter))
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
    matches
}

fn match_command(index: usize, command: Command, filter: &str) -> Option<MatchedCommand> {
    if filter.is_empty() {
        return Some(MatchedCommand {
            index,
            score: 0,
            title_indices: Vec::new(),
            description_indices: Vec::new(),
            action_indices: Vec::new(),
        });
    }
    let title = overlay::fuzzy_match_info(command.title(), filter);
    let action = overlay::fuzzy_match_info(command.action(), filter);
    let description = overlay::fuzzy_match_info(command.description(), filter);
    let score = title
        .as_ref()
        .map(|matched| matched.score + 5_000)
        .into_iter()
        .chain(action.as_ref().map(|matched| matched.score + 3_000))
        .chain(description.as_ref().map(|matched| matched.score + 1_000))
        .max()?;
    Some(MatchedCommand {
        index,
        score,
        title_indices: title.map_or_else(Vec::new, |matched| matched.indices),
        description_indices: description.map_or_else(Vec::new, |matched| matched.indices),
        action_indices: action.map_or_else(Vec::new, |matched| matched.indices),
    })
}
