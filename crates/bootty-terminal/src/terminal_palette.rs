use libghostty_vt::style::RgbColor;

pub type Palette = [RgbColor; 256];

pub fn generate_256_palette(
    base: &Palette,
    skip: &[bool; 256],
    bg: RgbColor,
    fg: RgbColor,
    harmonious: bool,
) -> Palette {
    let mut out = *base;
    for index in 16..256 {
        if skip[index] {
            continue;
        }
        out[index] = ansi_cube_color(index, bg, fg, harmonious);
    }
    out
}

fn ansi_cube_color(index: usize, bg: RgbColor, fg: RgbColor, harmonious: bool) -> RgbColor {
    if harmonious {
        if index == 16 {
            return bg;
        }
        if index == 231 {
            return fg;
        }
    }

    if (16..=231).contains(&index) {
        let value = index - 16;
        let r = value / 36;
        let g = (value / 6) % 6;
        let b = value % 6;
        return RgbColor {
            r: cube_component(r),
            g: cube_component(g),
            b: cube_component(b),
        };
    }

    if (232..=255).contains(&index) {
        let gray_index = if harmonious { 255 - index } else { index };
        let gray = 8 + ((gray_index - 232) as u8) * 10;
        return RgbColor {
            r: gray,
            g: gray,
            b: gray,
        };
    }

    RgbColor::default()
}

fn cube_component(value: usize) -> u8 {
    if value == 0 { 0 } else { 55 + value as u8 * 40 }
}
