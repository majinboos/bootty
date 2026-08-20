use bootty_font::{FontFeature, parse_font_features};

#[test]
fn single_feature_parser_preserves_harfbuzz_compatible_forms() {
    let kern_on = FontFeature::new(*b"kern", 1);
    for setting in [
        "kern",
        "kern, ",
        "kern on",
        "kern on, ",
        "+kern",
        "+kern, ",
        "\"kern\" = 1",
        "\"kern\" = 1, ",
    ] {
        assert_eq!(FontFeature::parse(setting), Some(kern_on), "{setting}");
    }

    let kern_off = FontFeature::new(*b"kern", 0);
    for setting in [
        "kern off",
        "kern off, ",
        "-'kern'",
        "-'kern', ",
        "\"kern\" = 0",
        "\"kern\" = 0, ",
    ] {
        assert_eq!(FontFeature::parse(setting), Some(kern_off), "{setting}");
    }

    let aalt_2 = FontFeature::new(*b"aalt", 2);
    for setting in ["aalt=2", "aalt=2, ", "'aalt' 2", "'aalt'\t2, "] {
        assert_eq!(FontFeature::parse(setting), Some(aalt_2), "{setting}");
    }

    assert_eq!(kern_on.to_string(), "+kern");
    assert_eq!(kern_off.to_string(), "-kern");
    assert_eq!(aalt_2.to_string(), "aalt=2");
}

#[test]
fn invalid_feature_forms_and_overflow_are_rejected() {
    for invalid in [
        "aalt=2x",
        "toolong",
        "sht",
        "-kern 1",
        "-kern on",
        "aalt=o,",
        "aalt=ofn,",
        "aalt=4294967296",
    ] {
        assert_eq!(FontFeature::parse(invalid), None, "{invalid}");
    }
}

#[test]
fn feature_list_parser_keeps_valid_values_in_source_order() {
    let kern_on = FontFeature::new(*b"kern", 1);
    let kern_off = FontFeature::new(*b"kern", 0);
    let aalt_2 = FontFeature::new(*b"aalt", 2);
    let features = parse_font_features(
        "  kern, kern on , +kern, \"kern\"  = 1,\
         kern    off, -'kern' , \"kern\"=0,\
         aalt=2,  'aalt'\t2,\
         aalt=2x, toolong, sht, -kern 1, -kern on, aalt=o, aalt=ofn,\
         last",
    );
    let expected = [
        vec![kern_on; 4],
        vec![kern_off; 3],
        vec![aalt_2; 2],
        vec![FontFeature::new(*b"last", 1)],
    ]
    .concat();

    assert_eq!(features, expected);
}
