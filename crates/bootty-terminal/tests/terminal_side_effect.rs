use std::sync::mpsc;

use bootty_terminal::terminal_side_effect::{
    TerminalSideEffect, TerminalSideEffectEvent, deliver_terminal_side_effects,
};
use pretty_assertions::assert_eq;

#[test]
fn delivery_adds_the_pane_identity_and_preserves_order() {
    let (sender, receiver) = mpsc::channel();
    let mut sender = Some(sender);
    let pane_id = Some("pane-7".to_owned());

    deliver_terminal_side_effects(
        &mut sender,
        &pane_id,
        vec![
            TerminalSideEffect::Bell,
            TerminalSideEffect::WindowTitle("build".to_owned()),
        ],
    );

    assert_eq!(
        receiver.try_iter().collect::<Vec<_>>(),
        vec![
            TerminalSideEffectEvent::new(Some("pane-7".to_owned()), TerminalSideEffect::Bell),
            TerminalSideEffectEvent::new(
                Some("pane-7".to_owned()),
                TerminalSideEffect::WindowTitle("build".to_owned()),
            ),
        ]
    );
}

#[test]
fn delivery_disables_a_disconnected_receiver() {
    let (sender, receiver) = mpsc::channel();
    drop(receiver);
    let mut sender = Some(sender);

    deliver_terminal_side_effects(
        &mut sender,
        &None,
        vec![TerminalSideEffect::Bell, TerminalSideEffect::FocusWindow],
    );

    assert!(sender.is_none());
}
