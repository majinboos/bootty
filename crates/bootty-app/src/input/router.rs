use eframe::egui;

use super::focus::InputFocus;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoutedInput {
    pub terminal_events: Vec<egui::Event>,
    pub ui_events: Vec<egui::Event>,
}

pub fn route_events(focus: InputFocus, events: Vec<egui::Event>) -> RoutedInput {
    if focus.terminal_owns_input() {
        return RoutedInput {
            terminal_events: events,
            ui_events: Vec::new(),
        };
    }

    RoutedInput {
        terminal_events: Vec::new(),
        ui_events: events,
    }
}
