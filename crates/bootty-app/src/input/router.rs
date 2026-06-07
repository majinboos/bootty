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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn sidebar_focus_consumes_rapid_navigation_before_terminal_input() {
        let routed = route_events(
            InputFocus::Sidebar,
            vec![
                key(egui::Key::J),
                key(egui::Key::K),
                key(egui::Key::ArrowDown),
                key(egui::Key::ArrowUp),
            ],
        );

        assert!(routed.terminal_events.is_empty());
        assert_eq!(routed.ui_events.len(), 4);
    }

    #[test]
    fn terminal_focus_leaves_events_for_terminal_input() {
        let routed = route_events(InputFocus::Terminal, vec![key(egui::Key::J)]);

        assert_eq!(routed.terminal_events.len(), 1);
        assert!(routed.ui_events.is_empty());
    }
}
