use bootty_ui::{Theme, ThemePalette};
use eframe::egui::{self, CornerRadius, Pos2, Rect, Stroke, UiBuilder};

use crate::{
    project_catalog::{ProjectPickerEntry, discover_project_picker_entries},
    strings::{display_path, session_name_for_path},
    worktree_catalog::{WorktreePickerEntry, discover_worktree_picker_entries},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMuxSessionRequest {
    pub session_id: String,
    pub cwd: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMuxSessionDialog {
    step: NewMuxSessionStep,
    filter: String,
    selected: usize,
    projects: Vec<ProjectPickerEntry>,
    worktrees: Vec<WorktreePickerEntry>,
    selected_project: Option<ProjectPickerEntry>,
    focus_filter: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewMuxSessionStep {
    Project,
    Worktree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewSessionPickerEvent {
    None,
    Close,
    NewWorktreeUnavailable,
    CreateSession(NewMuxSessionRequest),
}

impl NewMuxSessionDialog {
    pub fn open() -> Self {
        Self {
            step: NewMuxSessionStep::Project,
            filter: String::new(),
            selected: 0,
            projects: discover_project_picker_entries(),
            worktrees: Vec::new(),
            selected_project: None,
            focus_filter: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> NewSessionPickerEvent {
        let palette = theme.palette;
        let mut event = NewSessionPickerEvent::None;
        let mut selected_project: Option<ProjectPickerEntry> = None;
        let mut selected_worktree: Option<WorktreePickerEntry> = None;
        egui::Area::new(egui::Id::new("new-mux-session-dialog"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                bootty_ui::configure_style(ui.style_mut(), theme);
                let size = picker_panel_size(ctx, self.step);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, CornerRadius::ZERO, palette.mantle);
                painter.rect_stroke(
                    rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, palette.border),
                    egui::StrokeKind::Inside,
                );

                let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 52.0));
                painter.text(
                    Pos2::new(header.min.x + 16.0, header.center().y),
                    egui::Align2::LEFT_CENTER,
                    self.title(),
                    egui::FontId::monospace(15.0),
                    palette.warning,
                );
                painter.line_segment(
                    [
                        Pos2::new(rect.min.x + 16.0, header.max.y),
                        Pos2::new(rect.max.x - 16.0, header.max.y),
                    ],
                    Stroke::new(1.0, palette.border),
                );

                let filter_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x + 16.0, header.max.y + 10.0),
                    Pos2::new(rect.max.x - 16.0, header.max.y + 36.0),
                );
                self.draw_filter(ctx, ui, filter_rect, theme, palette);

                let footer_height = 34.0;
                let list_rect = Rect::from_min_max(
                    Pos2::new(rect.min.x + 16.0, filter_rect.max.y + 8.0),
                    Pos2::new(rect.max.x - 16.0, rect.max.y - footer_height - 12.0),
                );
                self.handle_row_navigation(ui);
                match self.step {
                    NewMuxSessionStep::Project => {
                        let entries = filtered_project_entries(&self.projects, &self.filter);
                        if ui.input(|input| input.key_pressed(egui::Key::Enter))
                            && let Some(entry) = entries.get(self.selected).cloned()
                        {
                            selected_project = Some(entry);
                        }
                        draw_project_picker_rows(ui, list_rect, palette, &entries, self.selected);
                    }
                    NewMuxSessionStep::Worktree => {
                        let entries = filtered_worktree_entries(&self.worktrees, &self.filter);
                        if ui.input(|input| input.key_pressed(egui::Key::Enter))
                            && let Some(entry) = entries.get(self.selected).cloned()
                        {
                            selected_worktree = Some(entry);
                        }
                        draw_worktree_picker_rows(ui, list_rect, palette, &entries, self.selected);
                    }
                }

                let footer = Rect::from_min_max(
                    Pos2::new(rect.min.x + 16.0, rect.max.y - footer_height),
                    Pos2::new(rect.max.x - 16.0, rect.max.y),
                );
                painter.line_segment(
                    [
                        Pos2::new(footer.min.x, footer.min.y),
                        Pos2::new(footer.max.x, footer.min.y),
                    ],
                    Stroke::new(1.0, palette.border),
                );
                draw_picker_footer(ui, footer, palette, self.step);
                if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                    event = NewSessionPickerEvent::Close;
                }
            });

        if let Some(project) = selected_project {
            self.open_worktrees(project);
        }
        if let Some(worktree) = selected_worktree {
            event = if worktree.is_new {
                NewSessionPickerEvent::NewWorktreeUnavailable
            } else if let Some(cwd) = worktree.path {
                NewSessionPickerEvent::CreateSession(NewMuxSessionRequest {
                    session_id: session_name_for_path(&cwd),
                    cwd,
                })
            } else {
                NewSessionPickerEvent::Close
            };
        }
        event
    }

    fn title(&self) -> &'static str {
        match self.step {
            NewMuxSessionStep::Project => "Project",
            NewMuxSessionStep::Worktree => "Worktree",
        }
    }

    fn draw_filter(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rect: Rect,
        theme: Theme,
        palette: ThemePalette,
    ) {
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                let filter_id = egui::Id::new("new-session-picker-filter");
                let response =
                    bootty_ui::flat_text_edit_singleline(ui, &mut self.filter, theme, |edit| {
                        edit.id(filter_id)
                            .desired_width(f32::INFINITY)
                            .hint_text("filter...")
                    });
                if self.focus_filter {
                    response.request_focus();
                    self.focus_filter = false;
                }
            },
        );
        let focus_color =
            if ctx.memory(|memory| memory.has_focus(egui::Id::new("new-session-picker-filter"))) {
                palette.accent
            } else {
                palette.border
            };
        ui.painter().line_segment(
            [
                Pos2::new(rect.min.x, rect.max.y - 2.0),
                Pos2::new(rect.max.x, rect.max.y - 2.0),
            ],
            Stroke::new(1.0, focus_color),
        );
    }

    fn handle_row_navigation(&mut self, ui: &egui::Ui) {
        let row_count = self.row_count();
        if row_count > 0 {
            let (next, previous) = ui.input(|input| {
                (
                    input.key_pressed(egui::Key::ArrowDown)
                        || input.key_pressed(egui::Key::N) && input.modifiers.ctrl,
                    input.key_pressed(egui::Key::ArrowUp)
                        || input.key_pressed(egui::Key::P) && input.modifiers.ctrl,
                )
            });
            self.selected =
                picker_selection_after_navigation(self.selected, row_count, next, previous);
        } else {
            self.selected = 0;
        }
    }

    fn row_count(&self) -> usize {
        match self.step {
            NewMuxSessionStep::Project => {
                filtered_project_entries(&self.projects, &self.filter).len()
            }
            NewMuxSessionStep::Worktree => {
                filtered_worktree_entries(&self.worktrees, &self.filter).len()
            }
        }
    }

    fn open_worktrees(&mut self, project: ProjectPickerEntry) {
        self.step = NewMuxSessionStep::Worktree;
        self.filter.clear();
        self.selected = 0;
        self.focus_filter = true;
        self.worktrees = discover_worktree_picker_entries(&project.path);
        self.selected_project = Some(project);
    }
}

fn picker_selection_after_navigation(
    selected: usize,
    row_count: usize,
    next: bool,
    previous: bool,
) -> usize {
    if row_count == 0 {
        return 0;
    }
    let mut selected = selected.min(row_count - 1);
    if next {
        selected = (selected + 1).min(row_count - 1);
    }
    if previous {
        selected = selected.saturating_sub(1);
    }
    selected
}

fn picker_panel_size(ctx: &egui::Context, step: NewMuxSessionStep) -> egui::Vec2 {
    let viewport = ctx.input(|input| input.content_rect().size());
    let desired = match step {
        NewMuxSessionStep::Project => egui::vec2(860.0, 560.0),
        NewMuxSessionStep::Worktree => egui::vec2(860.0, 430.0),
    };
    egui::vec2(
        desired.x.min((viewport.x - 72.0).max(560.0)),
        desired.y.min((viewport.y - 96.0).max(360.0)),
    )
}

fn filtered_project_entries(
    entries: &[ProjectPickerEntry],
    filter: &str,
) -> Vec<ProjectPickerEntry> {
    let filter = filter.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| {
            filter.is_empty()
                || display_path(&entry.path)
                    .to_ascii_lowercase()
                    .contains(&filter)
        })
        .cloned()
        .collect()
}

fn filtered_worktree_entries(
    entries: &[WorktreePickerEntry],
    filter: &str,
) -> Vec<WorktreePickerEntry> {
    let filter = filter.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| filter.is_empty() || entry.label.to_ascii_lowercase().contains(&filter))
        .cloned()
        .collect()
}

fn draw_project_picker_rows(
    ui: &egui::Ui,
    rect: Rect,
    palette: ThemePalette,
    entries: &[ProjectPickerEntry],
    selected: usize,
) {
    let painter = ui.painter_at(rect);
    let row_h = 30.0;
    let max = ((rect.height() / row_h).floor() as usize).min(entries.len());
    for (index, entry) in entries.iter().take(max).enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(rect.min.x, rect.min.y + index as f32 * row_h),
            egui::vec2(rect.width(), row_h),
        );
        draw_picker_row_background(&painter, row, palette, index == selected);
        painter.text(
            Pos2::new(row.min.x + 14.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            if entry.favorite { "★" } else { "✦" },
            egui::FontId::monospace(13.0),
            if index == selected {
                palette.warning
            } else {
                palette.muted
            },
        );
        painter.text(
            Pos2::new(row.min.x + 42.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            display_path(&entry.path),
            egui::FontId::monospace(13.0),
            if index == selected {
                palette.warning
            } else {
                palette.muted
            },
        );
    }
}

fn draw_worktree_picker_rows(
    ui: &egui::Ui,
    rect: Rect,
    palette: ThemePalette,
    entries: &[WorktreePickerEntry],
    selected: usize,
) {
    let painter = ui.painter_at(rect);
    let row_h = 30.0;
    let max = ((rect.height() / row_h).floor() as usize).min(entries.len());
    for (index, entry) in entries.iter().take(max).enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(rect.min.x, rect.min.y + index as f32 * row_h),
            egui::vec2(rect.width(), row_h),
        );
        draw_picker_row_background(&painter, row, palette, index == selected);
        painter.text(
            Pos2::new(row.min.x + 14.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            &entry.label,
            egui::FontId::monospace(13.0),
            if entry.is_new {
                palette.accent
            } else if index == selected {
                palette.warning
            } else {
                palette.muted
            },
        );
    }
}

fn draw_picker_row_background(
    painter: &egui::Painter,
    row: Rect,
    palette: ThemePalette,
    selected: bool,
) {
    if selected {
        painter.rect_filled(row, 0.0, palette.hover);
        painter.rect_filled(
            Rect::from_min_size(row.min, egui::vec2(2.0, row.height())),
            0.0,
            palette.accent,
        );
    }
}

fn draw_picker_footer(ui: &egui::Ui, rect: Rect, palette: ThemePalette, step: NewMuxSessionStep) {
    let painter = ui.painter_at(rect);
    let left = match step {
        NewMuxSessionStep::Project => {
            "ctrl-f ★  │  1 ~  │  2 ~/.config  │  3 ~/src  │  0 all  │  alt-enter worktree"
        }
        NewMuxSessionStep::Worktree => "enter select  │  esc cancel",
    };
    painter.text(
        Pos2::new(rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        left,
        egui::FontId::monospace(12.0),
        palette.muted,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_navigation_moves_down_and_up_without_wrapping() {
        assert_eq!(picker_selection_after_navigation(0, 3, true, false), 1);
        assert_eq!(picker_selection_after_navigation(2, 3, true, false), 2);
        assert_eq!(picker_selection_after_navigation(2, 3, false, true), 1);
        assert_eq!(picker_selection_after_navigation(0, 3, false, true), 0);
        assert_eq!(picker_selection_after_navigation(2, 0, true, true), 0);
    }
}
