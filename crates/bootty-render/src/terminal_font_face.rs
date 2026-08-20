#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphSize {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
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

pub(crate) fn is_symbol_codepoint(codepoint: u32) -> bool {
    matches!(
        codepoint,
        0x2190..=0x21FF
            | 0x2460..=0x24FF
            | 0x2500..=0x25FF
            | 0x2600..=0x27BF
            | 0x1F000..=0x1FAFF
            | 0xE000..=0xF8FF
            | 0xF0000..=0xFFFFD
            | 0x100000..=0x10FFFD
    )
}
