use bootty_winit::file_paths::format_file_paths_for_paste;
use pretty_assertions::assert_eq;
use std::path::Path;

#[test]
fn formats_paths_for_shell_paste() {
    for (paths, expected) in [
        (
            vec!["/tmp/Screen Shot's 1.png"],
            Some("'/tmp/Screen Shot'\\''s 1.png'"),
        ),
        (
            vec!["/tmp/a.png", "/tmp/b c.png"],
            Some("/tmp/a.png '/tmp/b c.png'"),
        ),
        (vec![], None),
    ] {
        assert_eq!(
            format_file_paths_for_paste(paths.into_iter().map(Path::new)),
            expected.map(str::to_owned),
        );
    }
}
