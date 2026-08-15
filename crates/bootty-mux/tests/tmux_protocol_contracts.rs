use bootty_mux::tmux_protocol::{
    TmuxClientSessionChangedNotification, TmuxControlNotification, TmuxControlParser,
    TmuxIdNameNotification, TmuxLayout, TmuxLayoutChangeNotification, TmuxLayoutContent,
    TmuxOutputNotification, TmuxParseError, TmuxSessionChangedNotification,
    TmuxWindowPaneChangedNotification, shell_quote, tmux_layout_checksum,
    tmux_layout_checksum_bytes, tmux_layout_checksum_string,
};

#[test]
fn tmux_layout_preserves_nested_panes_and_checksum_validation() {
    let single = TmuxLayout::parse("80x24,0,0,42").unwrap();
    assert_eq!(
        (single.width, single.height, single.x, single.y),
        (80, 24, 0, 0)
    );
    assert_eq!(single.content, TmuxLayoutContent::Pane(42));

    let nested =
        TmuxLayout::parse("80x24,0,0[80x12,0,0,1,80x12,0,12{40x12,0,12,2,40x12,40,12,3}]").unwrap();
    let TmuxLayoutContent::Vertical(children) = nested.content else {
        panic!("expected a vertical layout");
    };
    assert_eq!(children[0].content, TmuxLayoutContent::Pane(1));
    let TmuxLayoutContent::Horizontal(bottom) = &children[1].content else {
        panic!("expected a horizontal child layout");
    };
    assert_eq!(bottom[0].content, TmuxLayoutContent::Pane(2));
    assert_eq!(bottom[1].content, TmuxLayoutContent::Pane(3));

    for invalid in [
        "",
        "80x24,,0,1",
        "80x24,0,0,",
        "80x24,0,0{40x24,0,0,1",
        "80x24,0,0{40x24,0,0,1]",
        "80x24,0,0,1extra",
    ] {
        assert_eq!(
            TmuxLayout::parse(invalid),
            Err(TmuxParseError::SyntaxError),
            "{invalid}"
        );
    }

    let layout = "80x24,0,0{40x24,0,0,1,40x24,40,0,2}";
    assert_eq!(
        tmux_layout_checksum_string(tmux_layout_checksum(layout)),
        "f8f9"
    );
    assert_eq!(
        tmux_layout_checksum_string(tmux_layout_checksum_bytes(&[0xff; 8])),
        "03fc"
    );
    assert!(TmuxLayout::parse_with_checksum(&format!("f8f9,{layout}")).is_ok());
    assert_eq!(
        TmuxLayout::parse_with_checksum(&format!("0000,{layout}")),
        Err(TmuxParseError::ChecksumMismatch)
    );
}

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

#[test]
fn tmux_shell_arguments_are_single_quoted() {
    assert_eq!(shell_quote("foo'bar"), "'foo'\\''bar'");
}
