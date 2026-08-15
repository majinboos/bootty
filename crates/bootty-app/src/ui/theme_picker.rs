use std::path::Path;

use bootty_ui::Theme;
use eframe::egui;

use crate::ui::overlay::{self, FloatingWindow, ListRow, ListView, list};

#[derive(Clone, Debug)]
pub struct ThemePickerDialog {
    filter: String,
    selected: usize,
    focus_filter: bool,
    entries: Vec<ThemeEntry>,
    scope: ThemeScope,
    current: Option<String>,
    branch_label: String,
    last_preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemePickerEvent {
    None,
    Close,
    RestorePreview,
    Preview(String),
    Select(String),
}

#[derive(Clone, Debug)]
struct ThemeEntry {
    name: String,
    kind: ThemeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThemeKind {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThemeScope {
    All,
    Light,
    Dark,
}

impl ThemeScope {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    fn accepts(self, kind: ThemeKind) -> bool {
        match self {
            Self::All => true,
            Self::Light => kind == ThemeKind::Light,
            Self::Dark => kind == ThemeKind::Dark,
        }
    }
}

impl ThemePickerDialog {
    pub fn open(config_path: &Path, current: Option<&str>, branch_label: &str) -> Self {
        Self {
            filter: String::new(),
            selected: 0,
            focus_filter: true,
            entries: available_theme_entries(config_path),
            scope: ThemeScope::All,
            current: current.map(str::to_owned),
            branch_label: branch_label.to_owned(),
            last_preview: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> ThemePickerEvent {
        let tab = ctx.input(|input| input.key_pressed(egui::Key::Tab));
        if tab {
            self.scope = self.scope.next();
            self.selected = 0;
            self.last_preview = None;
        }

        let (rows, row_entries) = rows_for(
            &self.entries,
            &self.filter,
            self.scope,
            self.current.as_deref(),
            &self.branch_label,
        );
        self.selected = list::clamp_selection(self.selected, rows.len());
        if self.focus_filter
            && let Some(current) = self.current.as_deref()
            && let Some(index) = row_entries.iter().position(|entry| {
                entry
                    .and_then(|entry| self.entries.get(entry))
                    .is_some_and(|entry| entry.name == current)
            })
        {
            self.selected = index;
        }
        let list_max = overlay::list_max_height(ctx, 220.0, 560.0);
        let scroll_selected = self.focus_filter;

        let result = FloatingWindow::new("theme-picker-dialog", "Switch Theme")
            .icon("palette")
            .hint("Tab all/light/dark   Enter select   Esc close")
            .footer(format!(
                "{} themes · {}",
                self.entries.len(),
                self.scope.label()
            ))
            .width(overlay::panel_width(ctx, 760.0, 480.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("theme-picker-filter"),
                    &mut self.filter,
                    theme,
                    "filter themes...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.colored_label(palette.muted, "Showing");
                    for scope in [ThemeScope::All, ThemeScope::Light, ThemeScope::Dark] {
                        let selected = self.scope == scope;
                        let text = if selected {
                            egui::RichText::new(scope.label()).color(palette.base)
                        } else {
                            egui::RichText::new(scope.label()).color(palette.text)
                        };
                        let fill = if selected {
                            palette.primary
                        } else {
                            palette.surface
                        };
                        let response = egui::Frame::NONE
                            .fill(fill)
                            .corner_radius(egui::CornerRadius::same(5))
                            .inner_margin(egui::Margin::symmetric(8, 3))
                            .show(ui, |ui| ui.label(text))
                            .response
                            .interact(egui::Sense::click());
                        if response.clicked() {
                            self.scope = scope;
                            self.selected = 0;
                            self.last_preview = None;
                        }
                    }
                });
                ui.add_space(6.0);
                let outcome = ListView::new("theme-picker-list", &rows, self.selected)
                    .max_height(list_max)
                    .row_height(34.0)
                    .empty_text("no matching themes")
                    .scroll_selected(scroll_selected)
                    .show(ui, palette);
                self.selected = outcome.selected;
                let preview_row = outcome.hovered.unwrap_or(outcome.selected);
                let preview = row_entries
                    .get(preview_row)
                    .and_then(|entry| entry.and_then(|entry| self.entries.get(entry)))
                    .map(|entry| entry.name.clone());
                (outcome.activated, preview)
            });

        let (activated, preview) = result.inner;
        if let Some(index) = activated
            && let Some(theme) = row_entries
                .get(index)
                .and_then(|entry| entry.and_then(|entry| self.entries.get(entry)))
                .map(|entry| entry.name.clone())
        {
            return ThemePickerEvent::Select(theme);
        }
        if result.escaped || result.clicked_outside {
            return ThemePickerEvent::Close;
        }
        if let Some(preview) = preview {
            if self.current.as_deref() == Some(preview.as_str()) {
                if self.last_preview.take().is_some() {
                    return ThemePickerEvent::RestorePreview;
                }
            } else if self.last_preview.as_deref() != Some(preview.as_str()) {
                self.last_preview = Some(preview.clone());
                return ThemePickerEvent::Preview(preview);
            }
        }
        ThemePickerEvent::None
    }
}

pub fn available_themes(config_path: &Path) -> Vec<String> {
    available_theme_entries(config_path)
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn available_theme_entries(config_path: &Path) -> Vec<ThemeEntry> {
    let mut names: Vec<String> = crate::config::builtin_theme_names()
        .map(str::to_owned)
        .collect();
    if let Some(dir) = config_path.parent().map(|parent| parent.join("themes"))
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                names.push(stem.to_owned());
            }
        }
    }
    names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
        .into_iter()
        .map(|name| ThemeEntry {
            kind: classify_theme(&name),
            name,
        })
        .collect()
}

fn rows_for(
    entries: &[ThemeEntry],
    filter: &str,
    scope: ThemeScope,
    current: Option<&str>,
    branch_label: &str,
) -> (Vec<ListRow>, Vec<Option<usize>>) {
    let mut rows = Vec::new();
    let mut row_entries = Vec::new();
    let groups = match scope {
        ThemeScope::All => [Some(ThemeKind::Light), Some(ThemeKind::Dark)],
        ThemeScope::Light => [Some(ThemeKind::Light), None],
        ThemeScope::Dark => [Some(ThemeKind::Dark), None],
    };
    let matches = filtered(entries, filter, scope);
    for kind in groups.into_iter().flatten() {
        let section_matches = matches
            .iter()
            .filter(|matched| entries[matched.index].kind == kind)
            .collect::<Vec<_>>();
        if section_matches.is_empty() {
            continue;
        }
        rows.push(ListRow {
            primary: format!("{} themes", kind.label()),
            section: true,
            ..ListRow::default()
        });
        row_entries.push(None);
        for matched in section_matches {
            let entry = &entries[matched.index];
            rows.push(ListRow {
                icon: Some("palette".to_owned()),
                primary: entry.name.clone(),
                primary_matches: matched.name_indices.clone(),
                secondary: Some(format!("{} · {}", branch_label, entry.kind.label())),
                current: current == Some(entry.name.as_str()),
                ..ListRow::default()
            });
            row_entries.push(Some(matched.index));
        }
    }
    (rows, row_entries)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchedTheme {
    index: usize,
    score: i32,
    name_indices: Vec<usize>,
}

impl ThemeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

fn filtered(entries: &[ThemeEntry], filter: &str, scope: ThemeScope) -> Vec<MatchedTheme> {
    let filter = filter.trim();
    let mut matches = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| scope.accepts(entry.kind))
        .filter_map(|(index, entry)| {
            if filter.is_empty() {
                return Some(MatchedTheme {
                    index,
                    score: 0,
                    name_indices: Vec::new(),
                });
            }
            let matched = overlay::fuzzy_match_info(&entry.name, filter)?;
            Some(MatchedTheme {
                index,
                score: matched.score,
                name_indices: matched.indices,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
    matches
}

fn classify_theme(name: &str) -> ThemeKind {
    let name = name.to_ascii_lowercase();
    if ["light", "latte", "day", "dawn", "lotus", "solarized light"]
        .iter()
        .any(|needle| name.contains(needle))
    {
        ThemeKind::Light
    } else {
        ThemeKind::Dark
    }
}
