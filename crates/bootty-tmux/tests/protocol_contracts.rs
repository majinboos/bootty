use bootty_tmux::{
    TmuxClientSessionChangedNotification, TmuxControlNotification, TmuxControlParser,
    TmuxIdNameNotification, TmuxLayoutChangeNotification, TmuxOutputNotification,
    TmuxSessionChangedNotification, TmuxWindowPaneChangedNotification,
};

#[test]
fn tmux_control_parser_preserves_blocks_and_notifications() {
    fn feed(input: &str) -> Vec<TmuxControlNotification> {
        TmuxControlParser::default().put_str(input).unwrap()
    }

    assert_eq!(
        feed("%begin 1 1 1\nhello\nworld\n%end 1 1 1\n"),
        vec![TmuxControlNotification::BlockEnd("hello\nworld".to_owned())]
    );
    assert_eq!(
        feed("%begin 1 1 1\nproblem\n%error 1 1 1\n"),
        vec![TmuxControlNotification::BlockError("problem".to_owned())]
    );
    assert_eq!(
        feed("%output %42 foo bar baz\n"),
        vec![TmuxControlNotification::Output(TmuxOutputNotification {
            pane_id: 42,
            data: "foo bar baz".to_owned(),
        })]
    );
    assert_eq!(
        feed("%session-changed $42 foo\n"),
        vec![TmuxControlNotification::SessionChanged(
            TmuxSessionChangedNotification {
                id: 42,
                name: "foo".to_owned(),
            }
        )]
    );
    assert_eq!(
        feed("%layout-change @2 80x24,0,0,2 80x24,0,0,2 *-\n"),
        vec![TmuxControlNotification::LayoutChange(
            TmuxLayoutChangeNotification {
                window_id: 2,
                layout: "80x24,0,0,2".to_owned(),
                visible_layout: "80x24,0,0,2".to_owned(),
                raw_flags: "*-".to_owned(),
            }
        )]
    );
    assert_eq!(
        feed("%window-renamed @42 bar\n"),
        vec![TmuxControlNotification::WindowRenamed(
            TmuxIdNameNotification {
                id: 42,
                name: "bar".to_owned(),
            }
        )]
    );
    assert_eq!(
        feed("%window-pane-changed @42 %2\n"),
        vec![TmuxControlNotification::WindowPaneChanged(
            TmuxWindowPaneChangedNotification {
                window_id: 42,
                pane_id: 2,
            }
        )]
    );
    assert_eq!(
        feed("%client-session-changed /dev/pts/1 $2 mysession\n"),
        vec![TmuxControlNotification::ClientSessionChanged(
            TmuxClientSessionChangedNotification {
                client: "/dev/pts/1".to_owned(),
                session_id: 2,
                name: "mysession".to_owned(),
            }
        )]
    );
}
