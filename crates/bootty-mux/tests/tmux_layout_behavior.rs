use bootty_mux::{
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection},
    tmux_compatible_layout::{
        TmuxCompatibleLayoutParseError, parse, parse_with_checksum, tmux_layout_checksum_bytes,
        tmux_layout_checksum_string,
    },
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

#[rstest]
#[case("80x24,0,0,42", MuxPaneLayout::Pane("%42".to_owned()))]
#[case(
    "80x24,0,0[80x12,0,0,1,80x12,0,12{40x12,0,12,2,40x12,40,12,3}]",
    MuxPaneLayout::Split {
        direction: MuxPaneSplitDirection::Down,
        ratio_millis: 500,
        first: Box::new(MuxPaneLayout::Pane("%1".to_owned())),
        second: Box::new(MuxPaneLayout::Split {
            direction: MuxPaneSplitDirection::Right,
            ratio_millis: 500,
            first: Box::new(MuxPaneLayout::Pane("%2".to_owned())),
            second: Box::new(MuxPaneLayout::Pane("%3".to_owned())),
        }),
    }
)]
fn parser_preserves_panes_split_direction_and_ratio(
    #[case] input: &str,
    #[case] expected: MuxPaneLayout,
) {
    assert_eq!(parse(input), Ok(expected));
}

#[rstest]
#[case("")]
#[case("80x24,,0,1")]
#[case("80x24,0,0,")]
#[case("80x24,0,0{40x24,0,0,1")]
#[case("80x24,0,0{40x24,0,0,1]")]
#[case("80x24,0,0,1extra")]
fn malformed_layout_is_a_syntax_error(#[case] input: &str) {
    assert_eq!(
        parse(input),
        Err(TmuxCompatibleLayoutParseError::SyntaxError)
    );
}

#[rstest]
#[case("f8f9,80x24,0,0{40x24,0,0,1,40x24,40,0,2}", true)]
#[case("0000,80x24,0,0{40x24,0,0,1,40x24,40,0,2}", false)]
fn prefixed_checksum_is_validated(#[case] input: &str, #[case] valid: bool) {
    let actual = parse_with_checksum(input);

    if valid {
        assert!(actual.is_ok(), "{actual:#?}");
    } else {
        assert_eq!(
            actual,
            Err(TmuxCompatibleLayoutParseError::ChecksumMismatch)
        );
    }
}

#[derive(Arbitrary, Debug)]
struct ChecksumAppend {
    prefix: Vec<u8>,
    suffix: u8,
    checksum: u16,
}

proptest! {
    /// Property: appending one byte rotates the previous checksum and adds that byte.
    #[test]
    fn checksum_follows_the_tmux_byte_recurrence(input in any::<ChecksumAppend>()) {
        let prefix_checksum = tmux_layout_checksum_bytes(&input.prefix);
        let expected = prefix_checksum.rotate_right(1).wrapping_add(u16::from(input.suffix));
        let mut complete = input.prefix;
        complete.push(input.suffix);
        let encoded = tmux_layout_checksum_string(input.checksum);

        prop_assert_eq!(tmux_layout_checksum_bytes(&complete), expected);
        prop_assert_eq!(encoded.len(), 4);
        prop_assert!(encoded.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        prop_assert_eq!(u16::from_str_radix(&encoded, 16), Ok(input.checksum));
    }
}
