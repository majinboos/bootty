use bootty_winit::file_paths::format_file_paths_for_paste;
use std::path::Path;

#[test]
fn formats_paths_for_shell_paste() {
    assert_eq!(
        format_file_paths_for_paste([Path::new("/tmp/image.png")]),
        Some("/tmp/image.png".to_owned())
    );
    assert_eq!(
        format_file_paths_for_paste([Path::new("/tmp/Screen Shot's 1.png")]),
        Some("'/tmp/Screen Shot'\\''s 1.png'".to_owned())
    );
    assert_eq!(
        format_file_paths_for_paste([Path::new("/tmp/a.png"), Path::new("/tmp/b c.png")]),
        Some("/tmp/a.png '/tmp/b c.png'".to_owned())
    );
    assert_eq!(format_file_paths_for_paste([]), None);
}
