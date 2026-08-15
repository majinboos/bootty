use bootty_app::{
    config::{ColorConfig, SegmentAlign},
    mux::controller::SpaceId,
    theme::theme_palette_from_colors,
    ui::chrome::{
        ResolvedItem, ResolvedSegment, STATUS_EDGE_PAD, SidebarSpaceSwipeState, SpaceSwitcherEvent,
        SpaceSwitcherItem, StatusBarModel, show_space_switcher, show_status_bar,
        status_bar_window_tab_row_count, status_bar_windows_intersect_x_range,
        take_sidebar_space_swipe,
    },
};
use bootty_ui::icons::install_icon_fonts;
use egui::{Event, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect, TouchPhase, Vec2};

fn space(id: i64, name: &str, active: bool) -> SpaceSwitcherItem {
    SpaceSwitcherItem {
        id: SpaceId::from_persistence(id),
        name: name.to_owned(),
        icon: "folder".to_owned(),
        color: [0x7a, 0xa2, 0xf7],
        active,
        error: None,
    }
}

fn wheel(delta: Vec2, phase: TouchPhase) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta,
        phase,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn window_tabs_move_to_another_row_when_the_notch_crosses_them() {
    let context = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 300.0));
    let segments = [ResolvedSegment {
        align: SegmentAlign::Left,
        source_slot: 0,
        items: vec![ResolvedItem {
            text: "1 alpha-with-long-name".to_owned(),
            module: "windows".to_owned(),
            ..ResolvedItem::default()
        }],
    }];

    context
        .run_ui(
            RawInput {
                screen_rect: Some(screen),
                ..RawInput::default()
            },
            |ui| {
                let bar = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 30.0));
                assert!(status_bar_windows_intersect_x_range(
                    ui,
                    bar,
                    &segments,
                    STATUS_EDGE_PAD,
                    (20.0, 40.0),
                ));
                assert!(!status_bar_windows_intersect_x_range(
                    ui,
                    bar,
                    &segments,
                    STATUS_EDGE_PAD,
                    (500.0, 540.0),
                ));
                assert_eq!(
                    status_bar_window_tab_row_count(
                        ui,
                        bar,
                        &segments,
                        STATUS_EDGE_PAD,
                        Some((20.0, 40.0)),
                    ),
                    2
                );
            },
        )
        .drop_without_applying_deltas();
}

#[test]
fn pressing_empty_status_chrome_starts_a_native_window_drag() {
    let context = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 300.0));
    let palette = theme_palette_from_colors(&ColorConfig::default());
    let show = |ui: &mut egui::Ui| {
        show_status_bar(
            ui,
            palette,
            StatusBarModel {
                segments: &[],
                tab_context: None,
                background: palette.base,
                left_padding: STATUS_EDGE_PAD,
                row_height: screen.height(),
                notch_x: None,
                tab_rows: 1,
                interaction_id: "global-status-drag-contract",
            },
        );
    };

    context
        .run_ui(
            RawInput {
                screen_rect: Some(screen),
                events: vec![Event::PointerMoved(Pos2::new(20.0, 15.0))],
                ..RawInput::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| show(ui));
            },
        )
        .drop_without_applying_deltas();
    let output = context.run_ui(
        RawInput {
            screen_rect: Some(screen),
            events: vec![Event::PointerButton {
                pos: Pos2::new(20.0, 15.0),
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            ..RawInput::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| show(ui));
        },
    );

    let root = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .expect("root viewport output");
    assert!(root.commands.contains(&egui::ViewportCommand::StartDrag));
    output.drop_without_applying_deltas();
}

#[test]
fn horizontal_space_swipes_switch_once_and_leave_vertical_scroll_available() {
    let context = egui::Context::default();
    let sidebar = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 200.0));
    let spaces = [space(1, "Work", true), space(2, "Review", false)];
    let mut state = SidebarSpaceSwipeState::default();
    let mut remaining_wheels = Vec::new();
    let mut selected = None;

    context
        .run_ui(
            RawInput {
                screen_rect: Some(sidebar),
                events: vec![
                    Event::PointerMoved(sidebar.center()),
                    wheel(egui::vec2(-12.0, 1.0), TouchPhase::Start),
                    wheel(egui::vec2(-12.0, 1.0), TouchPhase::Move),
                    wheel(egui::vec2(0.0, 12.0), TouchPhase::Move),
                ],
                ..RawInput::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    selected = take_sidebar_space_swipe(ui, sidebar, &spaces, &mut state);
                    remaining_wheels = ui.input(|input| {
                        input
                            .events
                            .iter()
                            .filter_map(|event| match event {
                                Event::MouseWheel { delta, .. } => Some(*delta),
                                _ => None,
                            })
                            .collect()
                    });
                });
            },
        )
        .drop_without_applying_deltas();

    assert_eq!(selected, Some(spaces[1].id));
    assert_eq!(remaining_wheels, [egui::vec2(0.0, 12.0)]);
}

#[test]
fn clicking_a_space_switcher_control_activates_that_space() {
    let context = egui::Context::default();
    install_icon_fonts(&context);
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 44.0));
    let spaces = [space(1, "Work", true), space(2, "Review", false)];
    let palette = theme_palette_from_colors(&ColorConfig::default());
    let show = |events: Vec<Event>| {
        let mut event = None;
        context
            .run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..RawInput::default()
                },
                |ui| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            event = show_space_switcher(ui, palette, &spaces, None);
                        });
                },
            )
            .drop_without_applying_deltas();
        event
    };
    let second = Pos2::new(120.0, 22.0);

    show(vec![Event::PointerMoved(second)]);
    show(vec![Event::PointerButton {
        pos: second,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    }]);
    let event = show(vec![Event::PointerButton {
        pos: second,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);

    assert_eq!(event, Some(SpaceSwitcherEvent::Activate(spaces[1].id)));
}
