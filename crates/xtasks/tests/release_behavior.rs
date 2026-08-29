use pretty_assertions::assert_eq;
use rstest::rstest;
use xtasks::release::{Bump, bumped_version, validate_notes};

const NOTES: &str =
    "## Features\n- Added tabs.\n\n## Fixes\n- Fixed input.\n\n## Breaking Changes\n- None.\n";

#[rstest]
#[case(Bump::Major, "2.0.0")]
#[case(Bump::Minor, "1.3.0")]
#[case(Bump::Patch, "1.2.4")]
fn bumps_semantic_versions(#[case] bump: Bump, #[case] expected: &str) {
    assert_eq!(bumped_version("1.2.3", bump).unwrap(), expected);
}

#[rstest]
#[case("1.2")]
#[case("1.2.3.4")]
#[case("1.2.beta")]
fn rejects_unsupported_versions(#[case] version: &str) {
    assert!(bumped_version(version, Bump::Patch).is_err());
}

#[rstest]
#[case(NOTES, true)]
#[case(
    "## Features\n\n## Fixes\n- Fixed.\n\n## Breaking Changes\n- None.\n",
    false
)]
#[case(
    "## Fixes\n- Fixed.\n\n## Features\n- Added.\n\n## Breaking Changes\n- None.\n",
    false
)]
#[case(
    "Preamble\n## Features\n- Added.\n\n## Fixes\n- Fixed.\n\n## Breaking Changes\n- None.\n",
    false
)]
fn validates_release_note_sections(#[case] notes: &str, #[case] valid: bool) {
    assert_eq!(validate_notes(notes).is_ok(), valid);
}
