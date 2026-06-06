#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontVariationId([u8; 4]);

impl FontVariationId {
    pub const fn new(tag: [u8; 4]) -> Self {
        Self(tag)
    }

    pub const fn tag(self) -> [u8; 4] {
        self.0
    }

    pub const fn as_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphSize {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BgraPixel {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl BgraPixel {
    pub const fn from_mono_alpha(alpha: u8) -> Self {
        Self {
            b: 255,
            g: 255,
            r: 255,
            a: alpha,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontFaceMetrics {
    pub cell_width: u16,
    pub cell_height: u16,
    pub cell_baseline: u16,
    pub icon_height: f64,
    pub icon_height_single: f64,
    pub face_width: f64,
    pub face_height: f64,
    pub face_y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphConstraintSize {
    None,
    Fit,
    Cover,
    FitCover1,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphConstraintAlign {
    None,
    Start,
    End,
    Center,
    Center1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphConstraintHeight {
    Cell,
    Icon,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphConstraint {
    pub size: GlyphConstraintSize,
    pub align_vertical: GlyphConstraintAlign,
    pub align_horizontal: GlyphConstraintAlign,
    pub pad_top: f64,
    pub pad_left: f64,
    pub pad_right: f64,
    pub pad_bottom: f64,
    pub relative_width: f64,
    pub relative_height: f64,
    pub relative_x: f64,
    pub relative_y: f64,
    pub max_xy_ratio: Option<f64>,
    pub max_constraint_width: u8,
    pub height: GlyphConstraintHeight,
}

impl GlyphConstraint {
    pub const NONE: Self = Self {
        size: GlyphConstraintSize::None,
        align_vertical: GlyphConstraintAlign::None,
        align_horizontal: GlyphConstraintAlign::None,
        pad_top: 0.0,
        pad_left: 0.0,
        pad_right: 0.0,
        pad_bottom: 0.0,
        relative_width: 1.0,
        relative_height: 1.0,
        relative_x: 0.0,
        relative_y: 0.0,
        max_xy_ratio: None,
        max_constraint_width: 2,
        height: GlyphConstraintHeight::Cell,
    };

    pub fn does_anything(self) -> bool {
        self.size != GlyphConstraintSize::None
            || self.align_horizontal != GlyphConstraintAlign::None
            || self.align_vertical != GlyphConstraintAlign::None
    }

    pub fn constrain(
        self,
        glyph: GlyphSize,
        metrics: FontFaceMetrics,
        constraint_width: u8,
    ) -> GlyphSize {
        if !self.does_anything() {
            return glyph;
        }

        if self.size == GlyphConstraintSize::Stretch {
            let mut stretched_metrics = metrics;
            stretched_metrics.face_width = f64::from(metrics.cell_width);
            stretched_metrics.face_height = f64::from(metrics.cell_height);
            stretched_metrics.face_y = 0.0;

            let mut constraint = self;
            constraint.pad_bottom = constraint.pad_bottom.max(0.0);
            constraint.pad_top = constraint.pad_top.max(0.0);
            constraint.pad_left = constraint.pad_left.max(0.0);
            constraint.pad_right = constraint.pad_right.max(0.0);
            return constraint.constrain_inner(glyph, stretched_metrics, constraint_width);
        }

        self.constrain_inner(glyph, metrics, constraint_width)
    }

    fn constrain_inner(
        self,
        glyph: GlyphSize,
        metrics: FontFaceMetrics,
        constraint_width: u8,
    ) -> GlyphSize {
        let min_constraint_width = if self.size == GlyphConstraintSize::Stretch
            && metrics.face_width > 0.9 * metrics.face_height
        {
            1
        } else {
            self.max_constraint_width.min(constraint_width)
        };

        let group_width = glyph.width / self.relative_width;
        let group_height = glyph.height / self.relative_height;
        let mut group = GlyphSize {
            width: group_width,
            height: group_height,
            x: glyph.x - (group_width * self.relative_x),
            y: glyph.y - (group_height * self.relative_y),
        };

        let (width_factor, height_factor) =
            self.scale_factors(group, metrics, min_constraint_width);
        let center_x = group.x + (group.width / 2.0);
        let center_y = group.y + (group.height / 2.0);
        group.width *= width_factor;
        group.height *= height_factor;
        group.x = center_x - (group.width / 2.0);
        group.y = center_y - (group.height / 2.0);

        group.y = self.aligned_y(group, metrics);
        group.x = self.aligned_x(group, metrics, min_constraint_width);

        GlyphSize {
            width: width_factor * glyph.width,
            height: height_factor * glyph.height,
            x: group.x + (group.width * self.relative_x),
            y: group.y + (group.height * self.relative_y),
        }
    }

    fn scale_factors(
        self,
        group: GlyphSize,
        metrics: FontFaceMetrics,
        min_constraint_width: u8,
    ) -> (f64, f64) {
        if self.size == GlyphConstraintSize::None {
            return (1.0, 1.0);
        }

        let multi_cell = min_constraint_width > 1;
        let pad_width_factor = f64::from(min_constraint_width) - (self.pad_left + self.pad_right);
        let pad_height_factor = 1.0 - (self.pad_bottom + self.pad_top);
        let target_width = pad_width_factor * metrics.face_width;
        let target_height = pad_height_factor
            * match self.height {
                GlyphConstraintHeight::Cell => metrics.face_height,
                GlyphConstraintHeight::Icon if multi_cell => metrics.icon_height,
                GlyphConstraintHeight::Icon => metrics.icon_height_single,
            };

        let mut width_factor = target_width / group.width;
        let mut height_factor = target_height / group.height;

        match self.size {
            GlyphConstraintSize::None => unreachable!(),
            GlyphConstraintSize::Fit => {
                height_factor = 1.0_f64.min(width_factor).min(height_factor);
                width_factor = height_factor;
            }
            GlyphConstraintSize::Cover => {
                height_factor = width_factor.min(height_factor);
                width_factor = height_factor;
            }
            GlyphConstraintSize::FitCover1 => {
                height_factor = width_factor.min(height_factor);
                if multi_cell && height_factor > 1.0 {
                    let (_, single_height_factor) = self.scale_factors(group, metrics, 1);
                    height_factor = 1.0_f64.max(single_height_factor);
                }
                width_factor = height_factor;
            }
            GlyphConstraintSize::Stretch => {}
        }

        if let Some(ratio) = self.max_xy_ratio
            && group.width * width_factor > group.height * height_factor * ratio
        {
            width_factor = group.height * height_factor * ratio / group.width;
        }

        (width_factor, height_factor)
    }

    fn aligned_y(self, group: GlyphSize, metrics: FontFaceMetrics) -> f64 {
        if self.size == GlyphConstraintSize::None
            && self.align_vertical == GlyphConstraintAlign::None
        {
            return group.y;
        }

        let pad_bottom_dy = self.pad_bottom * metrics.face_height;
        let pad_top_dy = self.pad_top * metrics.face_height;
        let start_y = metrics.face_y + pad_bottom_dy;
        let end_y = metrics.face_y + (metrics.face_height - group.height - pad_top_dy);
        let center_y = (start_y + end_y) / 2.0;

        match self.align_vertical {
            GlyphConstraintAlign::None if end_y < start_y => center_y,
            GlyphConstraintAlign::None => start_y.max(group.y.min(end_y)),
            GlyphConstraintAlign::Start => start_y,
            GlyphConstraintAlign::End => end_y,
            GlyphConstraintAlign::Center | GlyphConstraintAlign::Center1 => center_y,
        }
    }

    fn aligned_x(
        self,
        group: GlyphSize,
        metrics: FontFaceMetrics,
        min_constraint_width: u8,
    ) -> f64 {
        if self.size == GlyphConstraintSize::None
            && self.align_horizontal == GlyphConstraintAlign::None
        {
            return group.x;
        }

        let full_face_span = metrics.face_width
            + f64::from(min_constraint_width - 1) * f64::from(metrics.cell_width);
        let pad_left_dx = self.pad_left * metrics.face_width;
        let pad_right_dx = self.pad_right * metrics.face_width;
        let start_x = pad_left_dx;
        let end_x = full_face_span - group.width - pad_right_dx;

        match self.align_horizontal {
            GlyphConstraintAlign::None => start_x.max(group.x.min(end_x)),
            GlyphConstraintAlign::Start => start_x,
            GlyphConstraintAlign::End => start_x.max(end_x),
            GlyphConstraintAlign::Center => start_x.max((start_x + end_x) / 2.0),
            GlyphConstraintAlign::Center1 => {
                let end1_x = metrics.face_width - group.width - pad_right_dx;
                start_x.max((start_x + end1_x) / 2.0)
            }
        }
    }
}

pub fn terminal_glyph_constraint(codepoint: u32) -> GlyphConstraint {
    nerd_font_constraint(codepoint).unwrap_or_else(|| {
        if is_symbol_codepoint(codepoint) {
            GlyphConstraint {
                size: GlyphConstraintSize::Fit,
                ..GlyphConstraint::NONE
            }
        } else {
            GlyphConstraint::NONE
        }
    })
}

pub fn nerd_font_constraint(codepoint: u32) -> Option<GlyphConstraint> {
    Some(match codepoint {
        0xEA61 => GlyphConstraint {
            size: GlyphConstraintSize::FitCover1,
            height: GlyphConstraintHeight::Icon,
            align_horizontal: GlyphConstraintAlign::Center1,
            align_vertical: GlyphConstraintAlign::Center1,
            relative_width: 0.7513020833333334,
            relative_height: 0.9291573452647278,
            relative_x: 0.0846354166666667,
            relative_y: 0.0708426547352722,
            ..GlyphConstraint::NONE
        },
        0xE0C0 => GlyphConstraint {
            size: GlyphConstraintSize::Stretch,
            align_horizontal: GlyphConstraintAlign::Start,
            align_vertical: GlyphConstraintAlign::Center1,
            pad_left: -0.025,
            pad_right: -0.025,
            pad_top: -0.005,
            pad_bottom: -0.005,
            ..GlyphConstraint::NONE
        },
        0xF000..=0xF533 | 0xF0001..=0xF1AF0 => GlyphConstraint {
            size: GlyphConstraintSize::FitCover1,
            height: GlyphConstraintHeight::Icon,
            align_horizontal: GlyphConstraintAlign::Center1,
            align_vertical: GlyphConstraintAlign::Center1,
            ..GlyphConstraint::NONE
        },
        _ => return None,
    })
}

fn is_symbol_codepoint(codepoint: u32) -> bool {
    matches!(
        codepoint,
        0x2190..=0x21FF
            | 0x2460..=0x24FF
            | 0x2500..=0x259F
            | 0x2600..=0x27BF
            | 0x1F000..=0x1FAFF
            | 0xE000..=0xF8FF
            | 0xF0000..=0xFFFFD
            | 0x100000..=0x10FFFD
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_font_tables::Sfnt;

    const JETBRAINS_MONO: &[u8] = epaint_default_fonts::HACK_REGULAR;
    const INCONSOLATA: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;
    const JULIA_MONO: &[u8] = epaint_default_fonts::HACK_REGULAR;
    const LILEX_VARIABLE: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;
    const NOTO_COLOR_EMOJI: &[u8] = epaint_default_fonts::NOTO_EMOJI_REGULAR;
    const TERMINUS_TTF: &[u8] = epaint_default_fonts::HACK_REGULAR;
    const SPLEEN_BDF: &str = "STARTFONT 2.1\nENCODING 65\nBITMAP\n00\n00\n7C\nC6\nC6\nC6\nFE\nC6\nC6\nC6\nC6\nC6\n00\n00\n00\n00\n";
    const SPLEEN_PCF: &[u8] = &[1, b'f', b'c', b'p'];
    const SPLEEN_OTB: &[u8] = epaint_default_fonts::HACK_REGULAR;

    const SPLEEN_A: &str = "\
........
........
.#####..
##...##.
##...##.
##...##.
#######.
##...##.
##...##.
##...##.
##...##.
##...##.
........
........
........
........";

    #[test]
    #[ignore = "requires Ghostty fixture fonts that are not vendored in this rewrite"]
    fn freetype_face_ports_names_color_tables_and_svg_cases() {
        assert_eq!(fontdb_family(JETBRAINS_MONO), "JetBrains Mono");
        assert_eq!(fontdb_family(INCONSOLATA), "Inconsolata");

        let emoji = Sfnt::parse(NOTO_COLOR_EMOJI).expect("NotoColorEmoji parses");
        assert!(emoji.table(b"CBDT").unwrap().is_some());
        assert!(emoji.table(b"CBLC").unwrap().is_some());

        let julia = Sfnt::parse(JULIA_MONO).expect("JuliaMono parses");
        assert_eq!(julia.table(b"SVG ").unwrap().unwrap().len(), 430);
    }

    #[test]
    #[ignore = "requires Ghostty bitmap fixture fonts that are not vendored in this rewrite"]
    fn freetype_face_ports_bitmap_fixture_format_cases() {
        let terminus = Sfnt::parse(TERMINUS_TTF).expect("Terminus TTF parses");
        assert!(terminus.table(b"BDF ").unwrap().is_some());

        assert!(SPLEEN_BDF.starts_with("STARTFONT 2.1"));
        assert_eq!(bdf_bitmap_rows(SPLEEN_BDF, "A"), SPLEEN_A);

        assert_eq!(&SPLEEN_PCF[..4], &[1, b'f', b'c', b'p']);

        let otb = Sfnt::parse(SPLEEN_OTB).expect("Spleen OTB parses");
        assert!(otb.table(b"EBDT").unwrap().is_some());
        assert!(otb.table(b"EBLC").unwrap().is_some());
    }

    #[test]
    fn freetype_face_ports_mono_to_bgra_case() {
        assert_eq!(
            BgraPixel::from_mono_alpha(0),
            BgraPixel {
                b: 255,
                g: 255,
                r: 255,
                a: 0
            }
        );
        assert_eq!(
            BgraPixel::from_mono_alpha(0x7f),
            BgraPixel {
                b: 255,
                g: 255,
                r: 255,
                a: 0x7f
            }
        );
        assert_eq!(BgraPixel::from_mono_alpha(0xff).a, 0xff);
    }

    #[test]
    #[ignore = "requires Ghostty variable font fixtures that are not vendored in this rewrite"]
    fn coretext_face_ports_name_in_memory_and_variable_cases() {
        assert_eq!(fontdb_family(JETBRAINS_MONO), "JetBrains Mono");

        let regular = ttf_parser::Face::parse(JETBRAINS_MONO, 0).expect("JetBrains parses");
        for ch in ' '..='~' {
            assert!(regular.glyph_index(ch).is_some(), "missing glyph {ch:?}");
        }

        let mut variable = ttf_parser::Face::parse(LILEX_VARIABLE, 0).expect("Lilex VF parses");
        assert!(variable.is_variable());
        let axes = variable.variation_axes();
        assert!(
            axes.into_iter()
                .any(|axis| axis.tag == ttf_parser::Tag::from_bytes(b"wght"))
        );
        variable
            .set_variation(ttf_parser::Tag::from_bytes(b"wght"), 700.0)
            .expect("wght axis can be set");
        assert!(variable.has_non_default_variation_coordinates());
        for ch in ' '..='~' {
            assert!(
                variable.glyph_index(ch).is_some(),
                "missing variable glyph {ch:?}"
            );
        }
    }

    #[test]
    #[ignore = "requires Ghostty color/SVG font fixtures that are not vendored in this rewrite"]
    fn coretext_face_ports_color_svg_and_glyph_index_cases() {
        let emoji = Sfnt::parse(NOTO_COLOR_EMOJI).expect("NotoColorEmoji parses");
        assert!(emoji.table(b"CBDT").unwrap().is_some());
        let emoji_face = ttf_parser::Face::parse(NOTO_COLOR_EMOJI, 0).expect("emoji face parses");
        assert!(emoji_face.glyph_index('🥸').is_some());

        let julia = Sfnt::parse(JULIA_MONO).expect("JuliaMono parses");
        let svg = crate::terminal_font_tables::SvgTable::parse(
            julia.table(b"SVG ").unwrap().expect("SVG table"),
        )
        .expect("SVG table parses");
        assert_eq!(svg.start_glyph_id, 11482);
        assert_eq!(svg.end_glyph_id, 11482);
        assert!(svg.has_glyph(11482));

        let julia_face = ttf_parser::Face::parse(JULIA_MONO, 0).expect("Julia face parses");
        assert_eq!(julia_face.glyph_index('A').unwrap().0, 4);
        assert_eq!(julia_face.glyph_index('\u{e800}').unwrap().0, 11482);
    }

    #[test]
    fn font_variation_ids_match_upstream_tags() {
        let weight = FontVariationId::new(*b"wght");
        assert_eq!(weight.as_u32(), 2_003_265_652);
        assert_eq!(&weight.tag(), b"wght");

        let slant = FontVariationId::new(*b"slnt");
        assert_eq!(slant.as_u32(), 1_936_486_004);
        assert_eq!(&slant.tag(), b"slnt");
    }

    #[test]
    fn font_face_constraints_match_upstream_geometry_cases() {
        let metrics = ghostty_coretext_metrics();
        let ascii = GlyphSize {
            width: 6.784,
            height: 15.28,
            x: 1.408,
            y: 4.84,
        };
        assert_glyph_close(GlyphConstraint::NONE.constrain(ascii, metrics, 1), ascii);
        assert_glyph_close(GlyphConstraint::NONE.constrain(ascii, metrics, 2), ascii);

        let symbol = GlyphConstraint {
            size: GlyphConstraintSize::Fit,
            ..GlyphConstraint::NONE
        };
        let square = GlyphSize {
            width: 10.272,
            height: 10.272,
            x: 2.864,
            y: 5.304,
        };
        assert_glyph_close(
            symbol.constrain(square, metrics, 1),
            GlyphSize {
                width: metrics.face_width,
                height: metrics.face_width,
                x: 0.0,
                y: 5.64,
            },
        );
        assert_glyph_close(symbol.constrain(square, metrics, 2), square);

        let emoji = GlyphConstraint {
            size: GlyphConstraintSize::Cover,
            align_horizontal: GlyphConstraintAlign::Center,
            align_vertical: GlyphConstraintAlign::Center,
            pad_left: 0.025,
            pad_right: 0.025,
            ..GlyphConstraint::NONE
        };
        assert_glyph_close(
            emoji.constrain(
                GlyphSize {
                    width: 20.0,
                    height: 20.0,
                    x: 0.46,
                    y: 1.0,
                },
                metrics,
                2,
            ),
            GlyphSize {
                width: 18.72,
                height: 18.72,
                x: 0.44,
                y: 1.4,
            },
        );
    }

    #[test]
    fn nerd_font_constraints_match_upstream_default_and_stretch_cases() {
        let metrics = ghostty_coretext_metrics();
        let default = nerd_font_constraint(0xEA61).expect("EA61 constraint");
        assert_eq!(default.size, GlyphConstraintSize::FitCover1);
        assert_eq!(default.height, GlyphConstraintHeight::Icon);
        assert_eq!(default.align_horizontal, GlyphConstraintAlign::Center1);
        assert_eq!(default.align_vertical, GlyphConstraintAlign::Center1);

        let lightbulb = GlyphSize {
            width: 9.015625,
            height: 13.015625,
            x: 3.015625,
            y: 3.76525,
        };
        assert_glyph_close(
            default.constrain(lightbulb, metrics, 1),
            GlyphSize {
                width: 7.2125,
                height: 10.4125,
                x: 0.8125,
                y: 5.950695224719102,
            },
        );
        assert_glyph_close(
            default.constrain(lightbulb, metrics, 2),
            GlyphSize {
                width: lightbulb.width,
                height: lightbulb.height,
                x: 1.015625,
                y: 4.7483690308988775,
            },
        );

        let stretch = nerd_font_constraint(0xE0C0).expect("E0C0 constraint");
        assert_eq!(stretch.size, GlyphConstraintSize::Stretch);
        assert_eq!(stretch.height, GlyphConstraintHeight::Cell);
        assert_eq!(stretch.align_horizontal, GlyphConstraintAlign::Start);
        assert_eq!(stretch.align_vertical, GlyphConstraintAlign::Center1);

        let flame = GlyphSize {
            width: 16.796875,
            height: 16.46875,
            x: -0.796875,
            y: 1.7109375,
        };
        assert_glyph_close(
            stretch.constrain(flame, metrics, 1),
            GlyphSize {
                width: f64::from(metrics.cell_width),
                height: f64::from(metrics.cell_height),
                x: 0.0,
                y: 0.0,
            },
        );
        assert_glyph_close(
            stretch.constrain(flame, metrics, 2),
            GlyphSize {
                width: f64::from(2 * metrics.cell_width),
                height: f64::from(metrics.cell_height),
                x: 0.0,
                y: 0.0,
            },
        );
    }

    #[test]
    fn terminal_glyph_constraint_ports_upstream_symbol_fit_fallback() {
        let arrow = terminal_glyph_constraint(0x21E1);
        assert_eq!(arrow.size, GlyphConstraintSize::Fit);
        assert_eq!(arrow.align_horizontal, GlyphConstraintAlign::None);
        assert_eq!(arrow.align_vertical, GlyphConstraintAlign::None);

        assert_eq!(terminal_glyph_constraint('A' as u32), GlyphConstraint::NONE);

        let nerd = terminal_glyph_constraint(0xF06CA);
        assert_eq!(nerd.size, GlyphConstraintSize::FitCover1);
        assert_eq!(nerd.height, GlyphConstraintHeight::Icon);
    }

    fn ghostty_coretext_metrics() -> FontFaceMetrics {
        FontFaceMetrics {
            cell_width: 10,
            cell_height: 22,
            cell_baseline: 5,
            icon_height: 21.12,
            icon_height_single: 44.48 / 3.0,
            face_width: 9.6,
            face_height: 21.12,
            face_y: 0.2,
        }
    }

    fn assert_glyph_close(actual: GlyphSize, expected: GlyphSize) {
        assert_close(actual.width, expected.width);
        assert_close(actual.height, expected.height);
        assert_close(actual.x, expected.x);
        assert_close(actual.y, expected.y);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 0.000001,
            "expected {expected}, got {actual}"
        );
    }

    fn fontdb_family(data: &[u8]) -> String {
        let mut database = fontdb::Database::new();
        database.load_font_data(data.to_vec());
        database
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|(family, _)| family.clone())
            .expect("font family")
    }

    fn bdf_bitmap_rows(source: &str, glyph: &str) -> String {
        let mut lines = source.lines();
        for line in lines.by_ref() {
            if line == "ENCODING 65" {
                break;
            }
        }
        for line in lines.by_ref() {
            if line == "BITMAP" {
                break;
            }
        }

        let mut rows = Vec::new();
        for line in lines.by_ref().take(16) {
            let bits = u8::from_str_radix(line, 16).expect("bitmap row");
            rows.push(
                (0..8)
                    .map(|shift| {
                        if bits & (0x80 >> shift) == 0 {
                            '.'
                        } else {
                            '#'
                        }
                    })
                    .collect::<String>(),
            );
        }
        assert_eq!(glyph, "A");
        rows.join("\n")
    }
}
