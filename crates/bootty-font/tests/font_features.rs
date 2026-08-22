use bootty_font::{FontFeature, parse_font_features};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("kern", FontFeature::new(*b"kern", 1), "+kern")]
#[case("kern off", FontFeature::new(*b"kern", 0), "-kern")]
#[case("aalt=2", FontFeature::new(*b"aalt", 2), "aalt=2")]
#[case("'aalt'\t2, ", FontFeature::new(*b"aalt", 2), "aalt=2")]
fn accepts_harfbuzz_forms(
    #[case] source: &str,
    #[case] expected: FontFeature,
    #[case] canonical: &str,
) {
    let parsed = FontFeature::parse(source);
    assert_eq!(parsed, Some(expected), "source: {source:?}");
    assert_eq!(
        parsed.map(|feature| feature.to_string()),
        Some(canonical.to_owned())
    );
}

#[rstest]
#[case("aalt=2x")]
#[case("toolong")]
#[case("-kern 1")]
#[case("aalt=4294967296")]
fn rejects_malformed_or_overflowing_values(#[case] source: &str) {
    assert_eq!(FontFeature::parse(source), None, "source: {source:?}");
}

proptest! {
    /// A canonical numeric feature is a lossless representation of its tag and value.
    #[test]
    fn canonical_numeric_form_round_trips(
        tag in prop::array::uniform4(b'a'..=b'z'),
        value in 2_u32..=u32::MAX,
    ) {
        let source = format!("{}={value}", String::from_utf8(tag.to_vec()).expect("ASCII tag"));
        let parsed = FontFeature::parse(&source);

        prop_assert_eq!(parsed, Some(FontFeature::new(tag, value)));
        prop_assert_eq!(parsed.expect("valid canonical form").to_string(), source);
    }

    /// Parsing a comma-separated list is equivalent to independently parsing each item and
    /// discarding invalid items; source order and duplicates are preserved.
    #[test]
    fn list_parser_matches_independent_item_oracle(
        values in prop::collection::vec(prop::option::of(0_u32..=32), 0..32),
    ) {
        let fields = values
            .iter()
            .map(|value| value.map_or_else(|| "bad".to_owned(), |value| format!("kern={value}")))
            .collect::<Vec<_>>();
        let expected = values
            .into_iter()
            .flatten()
            .map(|value| FontFeature::new(*b"kern", value))
            .collect::<Vec<_>>();

        prop_assert_eq!(parse_font_features(&fields.join(",")), expected);
    }
}
