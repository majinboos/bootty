use bootty_ui::{Theme, icons};
use eframe::egui;

use bootty_terminal::terminal_engine::TerminalSearchDirection;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalFindResult {
    pub found: bool,
    pub active_index: Option<usize>,
    pub match_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalFindDialog {
    query: String,
    focus: bool,
    result: Option<TerminalFindResult>,
    enter_direction: Option<TerminalSearchDirection>,
    last_rect: Option<egui::Rect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalFindEvent {
    None,
    Close,
    FocusFind,
    FocusTerminal,
    Search {
        query: String,
        direction: TerminalSearchDirection,
    },
}

impl TerminalFindDialog {
    pub fn open(query: String) -> Self {
        Self::open_with_direction(query, TerminalSearchDirection::Next)
    }

    pub fn open_with_direction(query: String, direction: TerminalSearchDirection) -> Self {
        Self {
            query,
            focus: true,
            result: None,
            enter_direction: Some(direction),
            last_rect: None,
        }
    }

    pub fn last_rect(&self) -> Option<egui::Rect> {
        self.last_rect
    }

    pub fn set_result(&mut self, result: TerminalFindResult) {
        self.result = Some(result);
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> TerminalFindEvent {
        let mut event = TerminalFindEvent::None;
        let palette = theme.palette;
        let screen = ctx.input(|input| input.content_rect());
        let width = (screen.width() - 32.0).clamp(300.0, 380.0);

        let area = egui::Area::new(egui::Id::new("terminal-find-dialog"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 16.0))
            .show(ctx, |ui| {
                bootty_ui::configure_style(ui.style_mut(), theme);
                egui::Frame::new()
                    .fill(palette.pane.linear_multiply(0.96))
                    .stroke(egui::Stroke::new(1.0, palette.border))
                    .corner_radius(egui::CornerRadius::same(palette.radius))
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            let count_text = self.count_text();
                            let count_width = 48.0;
                            let button_width = 72.0;
                            let field_width =
                                (width - count_width - button_width - 18.0).max(120.0);
                            let response = bootty_ui::flat_text_edit_singleline(
                                ui,
                                &mut self.query,
                                theme,
                                |edit| {
                                    edit.id(egui::Id::new("terminal-find-field"))
                                        .desired_width(field_width)
                                        .hint_text("Find")
                                },
                            );
                            if self.focus {
                                response.request_focus();
                                self.focus = false;
                            }

                            if response.clicked() && event == TerminalFindEvent::None {
                                event = TerminalFindEvent::FocusFind;
                            }
                            let query = self.query.trim().to_owned();
                            if response.changed() {
                                event = TerminalFindEvent::Search {
                                    query: query.clone(),
                                    direction: TerminalSearchDirection::Current,
                                };
                            }
                            if response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter))
                                && !query.is_empty()
                            {
                                let direction = if ui.input(|input| input.modifiers.shift) {
                                    TerminalSearchDirection::Previous
                                } else {
                                    self.enter_direction
                                        .unwrap_or(TerminalSearchDirection::Next)
                                };
                                event = TerminalFindEvent::Search {
                                    query: query.clone(),
                                    direction,
                                };
                            }

                            let count_color = if self.result.is_some_and(|result| !result.found) {
                                palette.destructive
                            } else {
                                palette.muted
                            };
                            ui.add_sized(
                                egui::vec2(count_width, 22.0),
                                egui::Label::new(
                                    egui::RichText::new(count_text)
                                        .monospace()
                                        .size(12.0)
                                        .color(count_color),
                                ),
                            );

                            if icon_button(ui, theme, "chevron-up", "Find previous").clicked()
                                && !query.is_empty()
                            {
                                event = TerminalFindEvent::Search {
                                    query: query.clone(),
                                    direction: TerminalSearchDirection::Previous,
                                };
                            }
                            if icon_button(ui, theme, "chevron-down", "Find next").clicked()
                                && !query.is_empty()
                            {
                                event = TerminalFindEvent::Search {
                                    query: query.clone(),
                                    direction: TerminalSearchDirection::Next,
                                };
                            }
                            if icon_button(ui, theme, "x", "Close find").clicked() {
                                event = TerminalFindEvent::Close;
                            }
                        });
                    });
            });
        self.last_rect = Some(area.response.rect);
        if event == TerminalFindEvent::None
            && ctx.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|pos| !area.response.rect.contains(pos))
            })
        {
            event = TerminalFindEvent::FocusTerminal;
        }

        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            return TerminalFindEvent::Close;
        }
        event
    }

    fn count_text(&self) -> String {
        match self.result {
            Some(result) if result.match_count > 0 => {
                let index = result.active_index.unwrap_or(1);
                format!("{index}/{}", result.match_count)
            }
            Some(_) => "0/0".to_owned(),
            None => String::new(),
        }
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    theme: Theme,
    slug: &str,
    tooltip: &'static str,
) -> egui::Response {
    let palette = theme.palette;
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(22.0), egui::Sense::click());
    let fill = if response.hovered() {
        palette.hover
    } else {
        palette.pane
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(palette.radius), fill);
    if !icons::paint_icon_slug(ui.painter(), slug, rect.center(), 14.0, palette.text) {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            slug,
            egui::FontId::monospace(11.0),
            palette.text,
        );
    }
    response.on_hover_text(tooltip)
}
