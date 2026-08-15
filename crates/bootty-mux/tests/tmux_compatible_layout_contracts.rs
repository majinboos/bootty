use bootty_mux::{
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection},
    tmux_compatible_layout::{
        TmuxCompatibleLayoutParseError, parse, parse_with_checksum, tmux_layout_checksum,
        tmux_layout_checksum_bytes, tmux_layout_checksum_string,
    },
};

#[test]
fn tmux_layout_preserves_nested_panes_and_checksum_validation() {
    assert_eq!(
        parse("80x24,0,0,42").unwrap(),
        MuxPaneLayout::Pane("%42".to_owned())
    );

    let nested = parse("80x24,0,0[80x12,0,0,1,80x12,0,12{40x12,0,12,2,40x12,40,12,3}]").unwrap();
    let MuxPaneLayout::Split {
        direction: MuxPaneSplitDirection::Down,
        ratio_millis: 500,
        first,
        second,
    } = nested
    else {
        panic!("expected a vertical layout");
    };
    assert_eq!(*first, MuxPaneLayout::Pane("%1".to_owned()));
    let MuxPaneLayout::Split {
        direction: MuxPaneSplitDirection::Right,
        ratio_millis: 500,
        first,
        second,
    } = *second
    else {
        panic!("expected a horizontal child layout");
    };
    assert_eq!(*first, MuxPaneLayout::Pane("%2".to_owned()));
    assert_eq!(*second, MuxPaneLayout::Pane("%3".to_owned()));

    for invalid in [
        "",
        "80x24,,0,1",
        "80x24,0,0,",
        "80x24,0,0{40x24,0,0,1",
        "80x24,0,0{40x24,0,0,1]",
        "80x24,0,0,1extra",
    ] {
        assert_eq!(
            parse(invalid),
            Err(TmuxCompatibleLayoutParseError::SyntaxError),
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
    assert!(parse_with_checksum(&format!("f8f9,{layout}")).is_ok());
    assert_eq!(
        parse_with_checksum(&format!("0000,{layout}")),
        Err(TmuxCompatibleLayoutParseError::ChecksumMismatch)
    );
}
