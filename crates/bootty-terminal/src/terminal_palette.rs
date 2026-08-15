use libghostty_vt::style::RgbColor;

pub type Palette = [RgbColor; 256];

/// Bootty's built-in base 16 ANSI colors — the default terminal palette before user overrides.
/// Exposed so settings can seed its ANSI override grid from the colors the terminal actually uses
/// rather than a generic VGA palette.
pub fn default_base16() -> [RgbColor; 16] {
    [
        0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
        0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
    ]
    .map(rgb_from_u24)
}

fn rgb_from_u24(value: u32) -> RgbColor {
    RgbColor {
        r: ((value >> 16) & 0xff) as u8,
        g: ((value >> 8) & 0xff) as u8,
        b: (value & 0xff) as u8,
    }
}

pub fn generate_256_palette(
    base: &Palette,
    skip: &[bool; 256],
    bg: RgbColor,
    fg: RgbColor,
    harmonious: bool,
) -> Palette {
    let mut result = *base;
    let mut anchors = [
        Lab::from_rgb(bg),
        Lab::from_rgb(base[1]),
        Lab::from_rgb(base[2]),
        Lab::from_rgb(base[3]),
        Lab::from_rgb(base[4]),
        Lab::from_rgb(base[5]),
        Lab::from_rgb(base[6]),
        Lab::from_rgb(fg),
    ];

    let light_theme = anchors[7].l < anchors[0].l;
    if light_theme && !harmonious {
        anchors.swap(0, 7);
    }

    let mut index = 16;
    for red in 0..6 {
        let red_t = red as f32 / 5.0;
        let c0 = Lab::lerp(red_t, anchors[0], anchors[1]);
        let c1 = Lab::lerp(red_t, anchors[2], anchors[3]);
        let c2 = Lab::lerp(red_t, anchors[4], anchors[5]);
        let c3 = Lab::lerp(red_t, anchors[6], anchors[7]);
        for green in 0..6 {
            let green_t = green as f32 / 5.0;
            let c4 = Lab::lerp(green_t, c0, c1);
            let c5 = Lab::lerp(green_t, c2, c3);
            for blue in 0..6 {
                if !skip[index] {
                    result[index] = Lab::lerp(blue as f32 / 5.0, c4, c5).to_rgb();
                }
                index += 1;
            }
        }
    }

    for step in 0..24 {
        if !skip[index] {
            result[index] = Lab::lerp((step + 1) as f32 / 25.0, anchors[0], anchors[7]).to_rgb();
        }
        index += 1;
    }

    result
}

#[derive(Clone, Copy)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

impl Lab {
    fn from_rgb(rgb: RgbColor) -> Self {
        let mut red = f32::from(rgb.r) / 255.0;
        let mut green = f32::from(rgb.g) / 255.0;
        let mut blue = f32::from(rgb.b) / 255.0;

        red = srgb_to_linear(red);
        green = srgb_to_linear(green);
        blue = srgb_to_linear(blue);

        let mut x = (red * 0.412_456_4 + green * 0.357_576_1 + blue * 0.180_437_5) / 0.950_47;
        let mut y = red * 0.212_672_9 + green * 0.715_152_2 + blue * 0.072_175;
        let mut z = (red * 0.019_333_9 + green * 0.119_192 + blue * 0.950_304_1) / 1.088_83;

        x = xyz_to_lab_curve(x);
        y = xyz_to_lab_curve(y);
        z = xyz_to_lab_curve(z);

        Self {
            l: 116.0 * y - 16.0,
            a: 500.0 * (x - y),
            b: 200.0 * (y - z),
        }
    }

    fn to_rgb(self) -> RgbColor {
        let y = (self.l + 16.0) / 116.0;
        let x = self.a / 500.0 + y;
        let z = y - self.b / 200.0;

        let x3 = x * x * x;
        let y3 = y * y * y;
        let z3 = z * z * z;
        let x = lab_to_xyz_curve(x, x3) * 0.950_47;
        let y = lab_to_xyz_curve(y, y3);
        let z = lab_to_xyz_curve(z, z3) * 1.088_83;

        let red = x * 3.240_454_2 - y * 1.537_138_5 - z * 0.498_531_4;
        let green = -x * 0.969_266 + y * 1.876_010_8 + z * 0.041_556;
        let blue = x * 0.055_643_4 - y * 0.204_025_9 + z * 1.057_225_2;

        RgbColor {
            r: linear_to_srgb_byte(red),
            g: linear_to_srgb_byte(green),
            b: linear_to_srgb_byte(blue),
        }
    }

    fn lerp(t: f32, a: Self, b: Self) -> Self {
        Self {
            l: a.l + t * (b.l - a.l),
            a: a.a + t * (b.a - a.a),
            b: a.b + t * (b.b - a.b),
        }
    }
}

fn srgb_to_linear(value: f32) -> f32 {
    if value > 0.040_45 {
        ((value + 0.055) / 1.055).powf(2.4)
    } else {
        value / 12.92
    }
}

fn xyz_to_lab_curve(value: f32) -> f32 {
    if value > 0.008_856 {
        value.cbrt()
    } else {
        7.787 * value + 16.0 / 116.0
    }
}

fn lab_to_xyz_curve(value: f32, cubed: f32) -> f32 {
    if cubed > 0.008_856 {
        cubed
    } else {
        (value - 16.0 / 116.0) / 7.787
    }
}

fn linear_to_srgb_byte(value: f32) -> u8 {
    let srgb = if value > 0.003_130_8 {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    } else {
        12.92 * value
    };
    (srgb.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}
