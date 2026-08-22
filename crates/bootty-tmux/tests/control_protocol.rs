use bootty_tmux::{
    TmuxClientSessionChangedNotification, TmuxControlNotification, TmuxControlParser,
    TmuxIdNameNotification, TmuxLayoutChangeNotification, TmuxOutputNotification,
    TmuxSessionChangedNotification, TmuxWindowPaneChangedNotification,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;
#[derive(Arbitrary, Debug)]
struct ControlBlock(
    #[proptest(strategy = "prop::collection::vec(\"[a-zA-Z0-9 ]{0,40}\", 0..20)")] Vec<String>,
);

#[rstest]
#[case::failed_block(
    "%begin 1 1 1\nproblem\n%error 1 1 1\n",
    vec![TmuxControlNotification::BlockError("problem".to_owned())],
)]
#[case::pane_output(
    "%output %42 foo bar baz\n",
    vec![TmuxControlNotification::Output(TmuxOutputNotification {
        pane_id: 42,
        data: "foo bar baz".to_owned(),
    })],
)]
#[case::session_change(
    "%session-changed $42 foo\n",
    vec![TmuxControlNotification::SessionChanged(TmuxSessionChangedNotification {
        id: 42,
        name: "foo".to_owned(),
    })],
)]
#[case::layout_change(
    "%layout-change @2 80x24,0,0,2 80x24,0,0,2 *-\n",
    vec![TmuxControlNotification::LayoutChange(TmuxLayoutChangeNotification {
        window_id: 2,
        layout: "80x24,0,0,2".to_owned(),
        visible_layout: "80x24,0,0,2".to_owned(),
        raw_flags: "*-".to_owned(),
    })],
)]
#[case::window_rename(
    "%window-renamed @42 bar\n",
    vec![TmuxControlNotification::WindowRenamed(TmuxIdNameNotification {
        id: 42,
        name: "bar".to_owned(),
    })],
)]
#[case::active_pane_change(
    "%window-pane-changed @42 %2\n",
    vec![TmuxControlNotification::WindowPaneChanged(TmuxWindowPaneChangedNotification {
        window_id: 42,
        pane_id: 2,
    })],
)]
#[case::client_session_change(
    "%client-session-changed /dev/pts/1 $2 mysession\n",
    vec![TmuxControlNotification::ClientSessionChanged(
        TmuxClientSessionChangedNotification {
            client: "/dev/pts/1".to_owned(),
            session_id: 2,
            name: "mysession".to_owned(),
        },
    )],
)]
fn parser_emits_the_observed_notification(
    #[case] input: &str,
    #[case] expected: Vec<TmuxControlNotification>,
) {
    let observed = TmuxControlParser::default()
        .put_str(input)
        .expect("parse control-mode input");
    assert_eq!(observed, expected);
}

proptest! {
    /// Property: completed blocks drop framing and leading empties, preserving remaining lines.
    #[test]
    fn completed_blocks_preserve_arbitrary_body_lines(ControlBlock(lines) in any::<ControlBlock>()) {
        let body = lines.join("\n");
        let framed = format!("%begin 1 1 1\n{body}\n%end 1 1 1\n");
        let expected = body.trim_start_matches('\n').to_owned();
        let observed = TmuxControlParser::default()
            .put_str(&framed)
            .expect("parse generated control block");
        assert_eq!(observed, vec![TmuxControlNotification::BlockEnd(expected)]);
    }
}
