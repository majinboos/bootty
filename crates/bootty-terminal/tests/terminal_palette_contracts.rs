use bootty_terminal::terminal_palette::{Palette, generate_256_palette};
use libghostty_vt::style::RgbColor;

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}
fn base() -> Palette {
    let mut value = [rgb(0, 0, 0); 256];
    for (index, color) in [
        rgb(0x45, 0x45, 0x5a),
        rgb(0xf3, 0x8b, 0xa8),
        rgb(0xa6, 0xe3, 0xa1),
        rgb(0xf9, 0xe2, 0xaf),
        rgb(0x89, 0xb4, 0xfa),
        rgb(0xf5, 0xc2, 0xe7),
        rgb(0x94, 0xe2, 0xd5),
        rgb(0xba, 0xc2, 0xde),
        rgb(0x58, 0x5b, 0x70),
        rgb(0xf3, 0x8b, 0xa8),
        rgb(0xa6, 0xe3, 0xa1),
        rgb(0xf9, 0xe2, 0xaf),
        rgb(0x89, 0xb4, 0xfa),
        rgb(0xf5, 0xc2, 0xe7),
        rgb(0x94, 0xe2, 0xd5),
        rgb(0xa6, 0xad, 0xcb),
    ]
    .into_iter()
    .enumerate()
    {
        value[index] = color;
    }
    value
}

#[test]
fn generated_palette_matches_known_answers() {
    let palette = generate_256_palette(
        &base(),
        &[false; 256],
        rgb(0x1e, 0x1e, 0x2e),
        rgb(0xcd, 0xd6, 0xf4),
        false,
    );
    assert_eq!(palette[16], rgb(0x1e, 0x1e, 0x2e));
    assert_eq!(palette[255], rgb(0xc5, 0xce, 0xeb));
}

#[test]
fn generated_palette_preserves_skipped_indexes() {
    let mut value = base();
    value[100] = rgb(1, 2, 3);
    let mut skip = [false; 256];
    skip[100] = true;
    let palette = generate_256_palette(
        &value,
        &skip,
        rgb(0x1e, 0x1e, 0x2e),
        rgb(0xcd, 0xd6, 0xf4),
        false,
    );
    assert_eq!(palette[100], rgb(1, 2, 3));
}

#[test]
fn harmonious_palette_preserves_light_theme_orientation() {
    let normal = generate_256_palette(
        &base(),
        &[false; 256],
        rgb(255, 255, 255),
        rgb(0, 0, 0),
        false,
    );
    let harmonious = generate_256_palette(
        &base(),
        &[false; 256],
        rgb(255, 255, 255),
        rgb(0, 0, 0),
        true,
    );
    assert_eq!(normal[16], rgb(0, 0, 0));
    assert_eq!(harmonious[16], rgb(255, 255, 255));
}
