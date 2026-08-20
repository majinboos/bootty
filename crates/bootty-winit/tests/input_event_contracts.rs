use bootty_winit::{
    direct_input::ModifierSideState,
    geometry::{CellMetrics, SurfaceRect, TerminalSurface, ViewTransform},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState, terminal_input_commands,
        terminal_input_commands_with_modifier_remaps, terminal_input_commands_with_options,
        terminal_input_commands_with_wheel_state,
    },
    modifier_remap::ModifierRemapSet,
    terminal::{KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, TerminalKey},
};
use eframe::egui::{self, Pos2, Rect, Vec2};

fn modifiers(ctrl: bool, alt: bool, command: bool) -> egui::Modifiers {
    egui::Modifiers {
        ctrl,
        alt,
        command,
        ..egui::Modifiers::default()
    }
}

fn surface(rect: Rect, cell: CellMetrics) -> TerminalSurface {
    TerminalSurface::for_rect(
        SurfaceRect {
            min_x: rect.min.x,
            min_y: rect.min.y,
            max_x: rect.max.x,
            max_y: rect.max.y,
        },
        cell,
    )
}

fn snapshot(events: Vec<egui::Event>) -> InputSnapshot {
    InputSnapshot {
        events,
        modifiers: egui::Modifiers::default(),
        modifier_sides: ModifierSideState::default(),
        hover_pos: None,
        pressed_mouse_button: None,
        surface: None,
        mouse_exclusion: None,
        view: ViewTransform::IDENTITY,
    }
}

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn text_ime_paste_and_focus_keep_distinct_commands() {
    let commands = terminal_input_commands(snapshot(vec![
        egui::Event::Text("a".to_owned()),
        egui::Event::Ime(egui::ImeEvent::Commit("é".to_owned())),
        egui::Event::Paste("hello".to_owned()),
        egui::Event::WindowFocused(true),
        egui::Event::WindowFocused(false),
    ]));

    assert_eq!(
        commands,
        vec![
            TerminalInputCommand::Text("a".to_owned()),
            TerminalInputCommand::Text("é".to_owned()),
            TerminalInputCommand::Paste("hello".to_owned()),
            TerminalInputCommand::Focus(true),
            TerminalInputCommand::Focus(false),
        ]
    );
}

#[test]
fn modified_keys_route_once_to_the_terminal_encoder() {
    let commands = terminal_input_commands(snapshot(vec![
        key_event(egui::Key::C, modifiers(true, false, false)),
        key_event(egui::Key::B, modifiers(false, true, false)),
        egui::Event::Text("b".to_owned()),
    ]));

    assert_eq!(
        commands,
        vec![
            TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::C,
                mods: KeyMods {
                    ctrl: true,
                    ..KeyMods::default()
                },
                repeat: false,
                utf8: Some("c"),
                unshifted: Some('c'),
            }),
            TerminalInputCommand::Key(KeyInput {
                key: TerminalKey::B,
                mods: KeyMods {
                    alt: true,
                    ..KeyMods::default()
                },
                repeat: false,
                utf8: Some("b"),
                unshifted: Some('b'),
            }),
        ]
    );
}

#[test]
fn unmodified_printable_key_waits_for_the_text_event() {
    let commands = terminal_input_commands(snapshot(vec![key_event(
        egui::Key::A,
        egui::Modifiers::default(),
    )]));

    assert!(commands.is_empty());
}

#[test]
fn option_as_alt_respects_the_selected_physical_side() {
    let option_events = vec![
        key_event(egui::Key::W, modifiers(false, true, false)),
        egui::Event::Text("∑".to_owned()),
    ];
    let mut left_option = snapshot(option_events.clone());
    left_option.modifier_sides.left_alt = true;
    let text = terminal_input_commands_with_options(
        left_option.clone(),
        &ModifierRemapSet::default(),
        MacosOptionAsAlt::None,
    );
    assert_eq!(text, vec![TerminalInputCommand::Text("∑".to_owned())]);

    let meta = terminal_input_commands_with_options(
        left_option,
        &ModifierRemapSet::default(),
        MacosOptionAsAlt::Left,
    );
    assert_eq!(
        meta,
        vec![TerminalInputCommand::Key(KeyInput {
            key: TerminalKey::W,
            mods: KeyMods {
                alt: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: Some("w"),
            unshifted: Some('w'),
        })]
    );

    let mut right_option = snapshot(option_events);
    right_option.modifier_sides.right_alt = true;
    assert_eq!(
        terminal_input_commands_with_options(
            right_option,
            &ModifierRemapSet::default(),
            MacosOptionAsAlt::Left,
        ),
        vec![TerminalInputCommand::Text("∑".to_owned())]
    );
}

#[test]
fn egui_input_applies_modifier_remaps_before_encoding() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("left_alt=right_ctrl").expect("remap parses");
    remaps.finalize();

    let commands = terminal_input_commands_with_modifier_remaps(
        snapshot(vec![key_event(egui::Key::B, modifiers(false, true, false))]),
        &remaps,
    );

    let [TerminalInputCommand::Key(input)] = commands.as_slice() else {
        panic!("expected one key command");
    };
    assert_eq!(
        input.mods,
        KeyMods {
            ctrl: true,
            right_ctrl: true,
            ..KeyMods::default()
        }
    );
}

#[test]
fn mouse_coordinates_are_relative_to_the_surface_and_inverse_view() {
    let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
    let terminal_surface = surface(rect, CellMetrics::new(10.0, 20.0));
    let view = ViewTransform {
        zoom: 2.0,
        pan_x: -15.0,
        pan_y: 12.0,
    };
    let logical = Pos2::new(55.0, 90.0);
    let rendered = Pos2::new(
        logical.x * view.zoom + view.pan_x,
        logical.y * view.zoom + view.pan_y,
    );
    let mut input = snapshot(vec![egui::Event::PointerButton {
        pos: rendered,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    }]);
    input.surface = Some(terminal_surface);
    input.view = view;

    let commands = terminal_input_commands(input);
    let [TerminalInputCommand::Mouse(mouse)] = commands.as_slice() else {
        panic!("expected one mouse command");
    };
    assert_eq!(mouse.action, MouseAction::Press);
    assert_eq!(mouse.button, Some(MouseButton::Left));
    assert_eq!(mouse.x, 35.0);
    assert_eq!(mouse.y, 50.0);
    assert_eq!(mouse.size.screen_width, 200);
    assert_eq!(mouse.size.screen_height, 160);
    assert_eq!(mouse.size.cell_width, 10);
    assert_eq!(mouse.size.cell_height, 20);
}

#[test]
fn mouse_motion_keeps_the_pressed_button() {
    let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
    let mut input = snapshot(vec![egui::Event::PointerMoved(Pos2::new(35.0, 70.0))]);
    input.hover_pos = Some(Pos2::new(35.0, 70.0));
    input.pressed_mouse_button = Some(MouseButton::Left);
    input.surface = Some(surface(rect, CellMetrics::new(9.0, 22.0)));

    let commands = terminal_input_commands(input);
    let [TerminalInputCommand::Mouse(mouse)] = commands.as_slice() else {
        panic!("expected one mouse command");
    };
    assert_eq!(mouse.action, MouseAction::Motion);
    assert_eq!(mouse.button, Some(MouseButton::Left));
    assert_eq!(mouse.x, 15.0);
    assert_eq!(mouse.y, 30.0);
}

#[test]
fn point_wheel_input_accumulates_until_one_cell() {
    let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
    let terminal_surface = surface(rect, CellMetrics::new(9.0, 22.0));
    let wheel = egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0.0, 11.0),
        modifiers: egui::Modifiers::default(),
        phase: egui::TouchPhase::Move,
    };
    let mut wheel_state = WheelScrollState::default();
    let make_snapshot = || {
        let mut input = snapshot(vec![wheel.clone()]);
        input.hover_pos = Some(Pos2::new(35.0, 70.0));
        input.surface = Some(terminal_surface);
        input
    };

    let first = terminal_input_commands_with_wheel_state(
        make_snapshot(),
        &ModifierRemapSet::default(),
        MacosOptionAsAlt::default(),
        &mut wheel_state,
    );
    let second = terminal_input_commands_with_wheel_state(
        make_snapshot(),
        &ModifierRemapSet::default(),
        MacosOptionAsAlt::default(),
        &mut wheel_state,
    );

    assert!(first.is_empty());
    let [
        TerminalInputCommand::MouseWheel {
            input,
            scroll_delta,
        },
    ] = second.as_slice()
    else {
        panic!("expected one wheel command");
    };
    assert_eq!(*scroll_delta, -1);
    assert_eq!(input.button, Some(MouseButton::Four));
}

#[test]
fn pointer_release_clamps_outside_the_surface_only_for_the_pressed_button() {
    let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
    let release = egui::Event::PointerButton {
        pos: Pos2::new(260.0, 170.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    };
    let mut tracked = snapshot(vec![release.clone()]);
    tracked.pressed_mouse_button = Some(MouseButton::Left);
    tracked.surface = Some(surface(rect, CellMetrics::new(9.0, 22.0)));

    let commands = terminal_input_commands(tracked);
    let [TerminalInputCommand::Mouse(mouse)] = commands.as_slice() else {
        panic!("expected one release command");
    };
    assert_eq!(mouse.action, MouseAction::Release);
    assert_eq!(mouse.x, 200.0);
    assert_eq!(mouse.y, 100.0);

    let mut untracked = snapshot(vec![release]);
    untracked.surface = Some(surface(rect, CellMetrics::new(9.0, 22.0)));
    assert!(terminal_input_commands(untracked).is_empty());
}

#[test]
fn mouse_exclusion_blocks_button_motion_and_wheel_events() {
    let terminal_rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
    let exclusion = Rect::from_min_max(Pos2::new(204.0, 40.0), Pos2::new(220.0, 140.0));
    let mut input = snapshot(vec![
        egui::Event::PointerButton {
            pos: Pos2::new(210.0, 70.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        },
        egui::Event::PointerMoved(Pos2::new(210.0, 72.0)),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 1.0),
            modifiers: egui::Modifiers::default(),
            phase: egui::TouchPhase::Move,
        },
    ]);
    input.hover_pos = Some(Pos2::new(210.0, 72.0));
    input.pressed_mouse_button = Some(MouseButton::Left);
    input.surface = Some(surface(terminal_rect, CellMetrics::new(9.0, 22.0)));
    input.mouse_exclusion = Some(exclusion);

    assert!(terminal_input_commands(input).is_empty());
}
