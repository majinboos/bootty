//! A searchable palette of app commands, opened with `command_palette` (default
//! `cmd+p`). Commands and their titles/descriptions come from the shared
//! [`crate::action_catalog`]; a choice dispatches through the same path as a
//! keybinding (see `app_actions::keybind_action_for_name`).

use std::collections::HashMap;

use bootty_ui::Theme;
use eframe::egui;

use crate::{
    action_catalog::Command,
    commands::CommandRegistry,
    ui::overlay::{self, FloatingWindow, ListRow, ListView, list},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaletteEntry {
    id: String,
    title: String,
    description: String,
    icon: String,
    action: Option<String>,
    core: Option<Command>,
}

#[derive(Clone, Debug)]
pub struct CommandPaletteDialog {
    filter: String,
    selected: usize,
    focus_filter: bool,
    /// Core actions plus every live extension descriptor in display order.
    commands: Vec<PaletteEntry>,
    /// dispatch-action string -> the chord it is bound to, for the trailing hint.
    bindings: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPaletteEvent {
    None,
    Close,
    Run(Command),
    RunExtension(String),
}

impl CommandPaletteDialog {
    /// `keybinds` is the active `chord=action` list, used to annotate each command
    /// with the key that triggers it.
    pub fn open(keybinds: &[String]) -> Self {
        Self::open_with_registry(keybinds, CommandRegistry::core())
    }

    pub fn open_with_registry(keybinds: &[String], registry: &CommandRegistry) -> Self {
        let mut bindings = HashMap::new();
        for raw in keybinds {
            if let Some((chord, action)) = overlay::parse_keybind(raw) {
                bindings.entry(action).or_insert(chord);
            }
        }
        let mut commands = CommandRegistry::core()
            .palette_commands()
            .map(|command| PaletteEntry {
                id: command.action().to_owned(),
                title: command.title().to_owned(),
                description: command.description().to_owned(),
                icon: command.icon().to_owned(),
                action: Some(command.action().to_owned()),
                core: Some(command),
            })
            .collect::<Vec<_>>();
        commands.extend(
            registry
                .extension_commands()
                .into_iter()
                .map(|descriptor| PaletteEntry {
                    id: descriptor.id,
                    title: descriptor.title,
                    description: descriptor.description,
                    icon: "extension".to_owned(),
                    action: None,
                    core: None,
                }),
        );
        Self {
            filter: String::new(),
            selected: 0,
            focus_filter: true,
            commands,
            bindings,
        }
    }

    /// The base action name of the row under the cursor, for "configure this
    /// command's keybinding" (`cmd+shift+,`).
    pub fn current_action(&self) -> Option<&str> {
        let matches = filtered(&self.commands, &self.filter);
        matches
            .get(self.selected)
            .and_then(|matched| self.commands.get(matched.index))
            .and_then(|entry| entry.action.as_deref())
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> CommandPaletteEvent {
        let matches = filtered(&self.commands, &self.filter);
        self.selected = list::clamp_selection(self.selected, matches.len());
        let rows: Vec<ListRow> = matches
            .iter()
            .filter_map(|matched| {
                self.commands
                    .get(matched.index)
                    .map(|command| (matched, command))
            })
            .map(|(matched, command)| {
                let trailing = command
                    .action
                    .as_deref()
                    .and_then(|action| self.bindings.get(action).cloned());
                ListRow {
                    icon: Some(command.icon.clone()),
                    primary: command.title.clone(),
                    primary_matches: matched.title_indices.clone(),
                    secondary: Some(command.description.clone()),
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
            && let Some(entry) = matches
                .get(index)
                .and_then(|matched| self.commands.get(matched.index))
        {
            if let Some(command) = entry.core {
                return CommandPaletteEvent::Run(command);
            }
            return CommandPaletteEvent::RunExtension(entry.id.clone());
        }
        if result.escaped || result.clicked_outside {
            return CommandPaletteEvent::Close;
        }
        CommandPaletteEvent::None
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

/// Commands matching `filter` (fuzzy over title, id, description), best-ranked first.
fn filtered(commands: &[PaletteEntry], filter: &str) -> Vec<MatchedCommand> {
    let filter = filter.trim();
    let mut matches = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match_command(index, command, filter))
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
    matches
}

fn match_command(index: usize, command: &PaletteEntry, filter: &str) -> Option<MatchedCommand> {
    if filter.is_empty() {
        return Some(MatchedCommand {
            index,
            score: 0,
            title_indices: Vec::new(),
            description_indices: Vec::new(),
            action_indices: Vec::new(),
        });
    }
    let title = overlay::fuzzy_match_info(&command.title, filter);
    let action = overlay::fuzzy_match_info(&command.id, filter);
    let description = overlay::fuzzy_match_info(&command.description, filter);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn palette_entries() -> Vec<PaletteEntry> {
        CommandRegistry::core()
            .palette_commands()
            .map(|command| PaletteEntry {
                id: command.action().to_owned(),
                title: command.title().to_owned(),
                description: command.description().to_owned(),
                icon: command.icon().to_owned(),
                action: Some(command.action().to_owned()),
                core: Some(command),
            })
            .collect()
    }

    #[test]
    fn filter_matches_title_action_or_description() {
        let commands = palette_entries();
        assert!(!filtered(&commands, "rename").is_empty());
        assert!(!filtered(&commands, "split").is_empty());
        assert!(filtered(&commands, "zzzznotacommand").is_empty());
        assert_eq!(filtered(&commands, "").len(), commands.len());
    }

    #[test]
    fn palette_includes_concrete_move_tab_commands() {
        let commands = palette_entries();
        let ids = commands
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&Command::MoveTabLeft.action()));
        assert!(ids.contains(&Command::MoveTabRight.action()));
        assert!(!ids.contains(&Command::MoveTab.action()));
    }

    #[test]
    fn filter_ranks_title_matches_before_description_matches() {
        let commands = palette_entries();
        let matches = filtered(&commands, "theme");
        let first = commands[matches[0].index].core;
        assert_eq!(first, Some(Command::SwitchTheme));
        assert!(!matches[0].title_indices.is_empty());
    }
}
