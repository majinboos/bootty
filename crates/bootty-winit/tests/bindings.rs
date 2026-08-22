use bootty_winit::{
    input_binding::{
        AdjustSelection, BindingAction, BindingElement, BindingFlags, BindingKey, BindingModSide,
        BindingMods, BindingParseError, BindingTrigger, CopyToClipboard, InputBinding,
        NavigateSearch, WriteScreen, WriteScreenAction, WriteScreenFormat, parse_action,
        parse_binding, parse_binding_elements,
    },
    input_binding_set::BindingSet,
    terminal::{KeyInput, KeyMods, TerminalKey},
};
use pretty_assertions::assert_eq;

fn key(key: TerminalKey, mods: KeyMods) -> KeyInput {
    KeyInput {
        key,
        mods,
        repeat: false,
        utf8: None,
        unshifted: None,
    }
}

#[test]
fn binding_parser_preserves_triggers_flags_and_modifier_sides() {
    for (input, mods, key) in [
        (
            "shift+ctrl+a=ignore",
            BindingMods {
                shift: true,
                ctrl: true,
                ..BindingMods::default()
            },
            BindingKey::Unicode('a'),
        ),
        (
            "ctrl++=ignore",
            BindingMods {
                ctrl: true,
                ..BindingMods::default()
            },
            BindingKey::Unicode('+'),
        ),
        (
            "alt+scroll_up=ignore",
            BindingMods {
                alt: true,
                ..BindingMods::default()
            },
            BindingKey::ScrollUp,
        ),
        (
            "alt+scroll_down=ignore",
            BindingMods {
                alt: true,
                ..BindingMods::default()
            },
            BindingKey::ScrollDown,
        ),
    ] {
        assert_eq!(
            parse_binding(input).expect("binding parses").trigger,
            BindingTrigger { mods, key },
            "{input}"
        );
    }

    let binding = parse_binding("unconsumed:performable:left_ctrl+right_alt+a=ignore")
        .expect("sided binding parses");
    assert_eq!(
        binding.trigger.mods,
        BindingMods {
            ctrl: true,
            alt: true,
            ctrl_side: Some(BindingModSide::Left),
            alt_side: Some(BindingModSide::Right),
            ..BindingMods::default()
        }
    );
    assert_eq!(binding.trigger.format_entry(), "left_ctrl+right_alt+a");
    assert_eq!(
        binding.flags,
        BindingFlags {
            consumed: false,
            performable: true,
            ..BindingFlags::default()
        }
    );

    for input in [
        "foo=ignore",
        "shift+shift+a=ignore",
        "a+b=ignore",
        "alt+right_alt+a=ignore",
    ] {
        assert_eq!(
            parse_binding(input),
            Err(BindingParseError::InvalidFormat),
            "{input}"
        );
    }
}

#[test]
fn binding_parser_preserves_physical_keys_aliases_and_catch_all() {
    for (input, key, canonical) in [
        ("KeyA", TerminalKey::A, "KeyA"),
        ("key_a", TerminalKey::A, "KeyA"),
        ("Enter", TerminalKey::Enter, "Enter"),
        ("enter", TerminalKey::Enter, "Enter"),
    ] {
        let parsed = parse_binding(&format!("{input}=ignore")).expect("physical key parses");
        assert_eq!(parsed.trigger.key, BindingKey::Physical(key), "{input}");
        assert_eq!(parsed.trigger.format_entry(), canonical, "{input}");
    }

    assert_eq!(
        parse_binding("physical:zero=ignore")
            .expect("legacy physical key parses")
            .trigger
            .key,
        BindingKey::Physical(TerminalKey::Digit0)
    );
    assert_eq!(
        parse_binding("ctrl+catch_all=ignore")
            .expect("catch-all parses")
            .trigger,
        BindingTrigger {
            mods: BindingMods {
                ctrl: true,
                ..BindingMods::default()
            },
            key: BindingKey::CatchAll,
        }
    );
    assert_eq!(
        parse_binding("Keya=ignore"),
        Err(BindingParseError::InvalidFormat)
    );
}

#[test]
fn binding_parser_preserves_action_grammar_and_errors() {
    for (input, action) in [
        ("a=ignore", BindingAction::Ignore),
        ("a=unbind", BindingAction::Unbind),
        ("a=reset", BindingAction::Reset),
        ("a=reload_config", BindingAction::ReloadConfig),
        ("a=new_window", BindingAction::NewWindow),
        ("a=close_window", BindingAction::CloseWindow),
        ("a=close_surface", BindingAction::CloseSurface),
        ("a=quit", BindingAction::Quit),
        ("a=toggle_fullscreen", BindingAction::ToggleFullscreen),
        ("a=open_settings", BindingAction::OpenSettings),
        ("a=csi:A", BindingAction::Csi("A".to_owned())),
        ("a=esc:7", BindingAction::Esc("7".to_owned())),
        ("a=text:=hello", BindingAction::Text("=hello".to_owned())),
        ("a=create_space", BindingAction::CreateSpace),
        ("a=close_space", BindingAction::CloseSpace),
        ("a=edit_space", BindingAction::EditSpace),
        ("a=next_space", BindingAction::NextSpace),
        ("a=previous_space", BindingAction::PreviousSpace),
        ("a=select_space:3", BindingAction::SelectSpace(3)),
        (
            "a=search:needle",
            BindingAction::Search("needle".to_owned()),
        ),
        ("a=search_selection", BindingAction::SearchSelection),
        (
            "a=navigate_search:previous",
            BindingAction::NavigateSearch(NavigateSearch::Previous),
        ),
        (
            "a=copy_to_clipboard:html",
            BindingAction::CopyToClipboard(CopyToClipboard::Html),
        ),
        (
            "a=increase_font_size:1.5",
            BindingAction::IncreaseFontSize(1.5),
        ),
        ("a=set_font_size:13.5", BindingAction::SetFontSize(13.5)),
        ("a=scroll_to_row:12", BindingAction::ScrollToRow(12)),
        (
            "a=scroll_page_fractional:-0.5",
            BindingAction::ScrollPageFractional(-0.5),
        ),
        (
            "a=scroll_page_lines:-10",
            BindingAction::ScrollPageLines(-10),
        ),
        (
            "a=adjust_selection:beginning_of_line",
            BindingAction::AdjustSelection(AdjustSelection::BeginningOfLine),
        ),
        ("a=jump_to_prompt:-1", BindingAction::JumpToPrompt(-1)),
        (
            "a=write_scrollback_file:paste,vt",
            BindingAction::WriteScrollbackFile(WriteScreen {
                action: WriteScreenAction::Paste,
                emit: WriteScreenFormat::Vt,
            }),
        ),
        (
            "a=write_screen_file:copy,html",
            BindingAction::WriteScreenFile(WriteScreen {
                action: WriteScreenAction::Copy,
                emit: WriteScreenFormat::Html,
            }),
        ),
        (
            "a=activate_key_table:copy-mode",
            BindingAction::ActivateKeyTable("copy-mode".to_owned()),
        ),
        (
            "a=toggle_mouse_reporting",
            BindingAction::ToggleMouseReporting,
        ),
    ] {
        assert_eq!(
            parse_binding(input)
                .expect("parameterized action parses")
                .action,
            action,
            "{input}"
        );
    }

    for input in [
        "a=nopenopenope",
        "a=ignore:A",
        "a=reset:A",
        "a=csi",
        "a=esc",
        "a=text",
        "a=copy_to_clipboard:invalid",
        "a=navigate_search:sideways",
        "a=increase_font_size:nope",
        "a=set_font_size:nan",
        "a=scroll_page_fractional:inf",
        "a=scroll_page_lines:100000",
        "a=adjust_selection:middle",
        "a=write_screen_file:copy,html,extra",
    ] {
        assert!(parse_binding(input).is_err(), "{input}");
    }
}

#[test]
fn binding_sequences_and_chains_round_trip_through_the_public_set() {
    assert_eq!(
        parse_binding_elements("ctrl+a>ctrl+b=ignore").expect("sequence parses"),
        vec![
            BindingElement::Leader(BindingTrigger {
                mods: BindingMods {
                    ctrl: true,
                    ..BindingMods::default()
                },
                key: BindingKey::Unicode('a'),
            }),
            BindingElement::Binding(InputBinding {
                trigger: BindingTrigger {
                    mods: BindingMods {
                        ctrl: true,
                        ..BindingMods::default()
                    },
                    key: BindingKey::Unicode('b'),
                },
                action: BindingAction::Ignore,
                flags: BindingFlags::default(),
            }),
        ]
    );

    let mut set = BindingSet::default();
    for entry in [
        "a=text:hello",
        "chain=text:world",
        "ctrl+b=reset",
        "c>d=csi:0m",
        "e>b=reset",
        "e>c=text:next",
        "e>b=text:updated",
    ] {
        set.parse_and_put(entry).expect("entry parses");
    }
    assert_eq!(
        set.format_entries(),
        vec![
            "a=text:hello",
            "chain=text:world",
            "ctrl+b=reset",
            "c>d=csi:0m",
            "e>c=text:next",
            "e>b=text:updated",
        ]
    );

    let cloned = set.clone_for_config();
    assert_eq!(cloned.format_entries(), set.format_entries());
}

#[test]
fn binding_set_preserves_lookup_precedence_and_reverse_lookup() {
    let mut set = BindingSet::default();
    for entry in [
        "ctrl+quote=ignore",
        "ctrl+'=reset",
        "catch_all=text:fallback",
        "ctrl+catch_all=csi:A",
    ] {
        set.parse_and_put(entry).expect("entry parses");
    }

    let ctrl = KeyMods {
        ctrl: true,
        ..KeyMods::default()
    };
    assert_eq!(
        set.get_event(key(TerminalKey::Quote, ctrl))
            .expect("physical binding wins")
            .action,
        BindingAction::Ignore
    );
    assert_eq!(
        set.get_event(KeyInput {
            utf8: Some("'"),
            ..key(TerminalKey::A, ctrl)
        })
        .expect("codepoint binding wins")
        .action,
        BindingAction::Reset
    );
    assert_eq!(
        set.get_event(KeyInput {
            unshifted: Some('A'),
            ..key(TerminalKey::J, ctrl)
        })
        .expect("modified catch-all wins")
        .action,
        BindingAction::Csi("A".to_owned())
    );
    assert_eq!(
        set.get_event(key(TerminalKey::A, KeyMods::default()))
            .expect("global catch-all wins")
            .action,
        BindingAction::Text("fallback".to_owned())
    );

    let mut reverse = BindingSet::default();
    reverse.parse_and_put("a=reset").expect("entry parses");
    reverse.parse_and_put("b=reset").expect("entry parses");
    assert_eq!(
        reverse
            .get_trigger(&BindingAction::Reset)
            .expect("latest binding is found")
            .key,
        BindingKey::Unicode('b')
    );
    reverse.parse_and_put("b=unbind").expect("unbind parses");
    assert_eq!(
        reverse
            .get_trigger(&BindingAction::Reset)
            .expect("prior binding is restored")
            .key,
        BindingKey::Unicode('a')
    );
}

#[test]
fn binding_set_preserves_sided_modifiers_unbind_and_invalid_chain_behavior() {
    let mut set = BindingSet::default();
    set.parse_and_put("alt+KeyA=text:any")
        .expect("generic binding parses");
    set.parse_and_put("right_alt+KeyA=text:right")
        .expect("sided binding parses");

    assert_eq!(
        set.get_event(key(
            TerminalKey::A,
            KeyMods {
                alt: true,
                right_alt: true,
                ..KeyMods::default()
            }
        ))
        .expect("sided binding wins")
        .action,
        BindingAction::Text("right".to_owned())
    );
    assert_eq!(
        set.get_event(key(
            TerminalKey::A,
            KeyMods {
                alt: true,
                ..KeyMods::default()
            }
        ))
        .expect("generic binding remains")
        .action,
        BindingAction::Text("any".to_owned())
    );

    let mut sequence = BindingSet::default();
    sequence
        .parse_and_put("a>b=text:leaf")
        .expect("sequence parses");
    sequence
        .parse_and_put("a>b=unbind")
        .expect("sequence unbind parses");
    assert!(sequence.format_entries().is_empty());

    let mut invalid = BindingSet::default();
    assert_eq!(
        invalid.parse_and_put("chain=text:orphan"),
        Err(BindingParseError::InvalidFormat)
    );
    invalid.parse_and_put("a=reset").expect("entry parses");
    assert_eq!(
        invalid.parse_and_put("chain=unbind"),
        Err(BindingParseError::InvalidFormat)
    );
}

#[test]
fn binding_action_grammar_preserves_defaults_and_validation() {
    for (input, action, canonical) in [
        (
            "copy_to_clipboard",
            BindingAction::CopyToClipboard(CopyToClipboard::Mixed),
            "copy_to_clipboard:mixed",
        ),
        (
            "write_screen_file:open",
            BindingAction::WriteScreenFile(WriteScreen {
                action: WriteScreenAction::Open,
                emit: WriteScreenFormat::Plain,
            }),
            "write_screen_file:open,plain",
        ),
        ("select_tab:1", BindingAction::SelectTab(1), "select_tab:1"),
        (
            "select_space:2",
            BindingAction::SelectSpace(2),
            "select_space:2",
        ),
        (
            "select_session:3",
            BindingAction::SelectSession(3),
            "select_session:3",
        ),
        (
            "set_surface_title:\u{1f47b}",
            BindingAction::SetSurfaceTitle("\u{1f47b}".to_owned()),
            "set_surface_title:\\xf0\\x9f\\x91\\xbb",
        ),
    ] {
        assert_eq!(parse_action(input), Ok(action.clone()), "{input}");
        assert_eq!(action.format_entry(), canonical, "{input}");
    }

    for input in [
        "ignore:value",
        "set_surface_title",
        "set_font_size:nan",
        "select_tab:0",
        "select_space:0",
        "select_session:0",
        "select_pane:sideways",
        "write_screen_file:copy,html,extra",
    ] {
        assert_eq!(
            parse_action(input),
            Err(BindingParseError::InvalidFormat),
            "{input}"
        );
    }
}
