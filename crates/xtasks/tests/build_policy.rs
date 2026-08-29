use pretty_assertions::assert_eq;
use rstest::rstest;
use xtasks::build::{BuildArgs, profile, profile_args};

#[rstest]
#[case(false, false, "dynamic-release", vec!["--profile", "dynamic-release"])]
#[case(true, false, "fast-release", vec!["--profile", "fast-release"])]
#[case(false, true, "release", vec!["--release"])]
#[case(true, true, "fast-release", vec!["--profile", "fast-release"])]
fn build_flags_select_the_legacy_profile(
    #[case] fast: bool,
    #[case] static_linkage: bool,
    #[case] expected_profile: &str,
    #[case] expected_args: Vec<&str>,
) {
    let args = BuildArgs {
        fast,
        static_linkage,
    };

    assert_eq!(profile(&args), expected_profile);
    assert_eq!(profile_args(&args), expected_args);
}
