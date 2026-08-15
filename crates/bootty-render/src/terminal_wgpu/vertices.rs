use crate::{
    geometry::SurfaceRect, paint_plan::PlanColor, terminal_image::KittyImagePlacement,
    terminal_text_atlas::TexturedGlyphQuad,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TerminalQuadDraw {
    pub(super) rect: SurfaceRect,
    pub(super) color: PlanColor,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BackgroundVertex {
    pub(super) position: [f32; 2],
    pub(super) color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextVertex {
    pub(super) position: [f32; 2],
    pub(super) uv: [f32; 2],
    pub(super) color: [f32; 4],
}

pub(super) fn background_quad_vertices(
    surface: SurfaceRect,
    draw: TerminalQuadDraw,
) -> [BackgroundVertex; 6] {
    let transform = SurfaceNdcTransform::new(surface);
    let min_x = transform.x(draw.rect.min_x);
    let max_x = transform.x(draw.rect.max_x);
    let min_y = transform.y(draw.rect.min_y);
    let max_y = transform.y(draw.rect.max_y);
    let top_left = [min_x, min_y];
    let top_right = [max_x, min_y];
    let bottom_right = [max_x, max_y];
    let bottom_left = [min_x, max_y];
    let color = color_to_float(draw.color);

    [
        BackgroundVertex {
            position: top_left,
            color,
        },
        BackgroundVertex {
            position: bottom_left,
            color,
        },
        BackgroundVertex {
            position: bottom_right,
            color,
        },
        BackgroundVertex {
            position: top_left,
            color,
        },
        BackgroundVertex {
            position: bottom_right,
            color,
        },
        BackgroundVertex {
            position: top_right,
            color,
        },
    ]
}

pub(super) fn text_vertices_into(
    surface: SurfaceRect,
    ppp: f32,
    quads: &[TexturedGlyphQuad],
    vertices: &mut Vec<TextVertex>,
) {
    vertices.reserve(quads.len() * 6);
    let transform = SurfaceNdcTransform::new(surface);
    // Snap each glyph's top and bottom to the physical pixel grid. `Fit rows to window` makes the
    // cell height fractional, so unsnapped rows land on sub-pixel boundaries and the glyph texels
    // straddle screen pixels — a couple pixels of vertical ghosting that grows down the column as
    // the fractional offset accumulates. Images already snap the same way; X is left exact because
    // column width stays integer. At an integer cell height the snap is a no-op.
    let snap_y = |y: f32| snap_to_grid(y, surface.min_y, ppp);
    for quad in quads {
        let min_x = transform.x(quad.rect.min_x);
        let max_x = transform.x(quad.rect.max_x);
        let min_y = transform.y(snap_y(quad.rect.min_y));
        let max_y = transform.y(snap_y(quad.rect.max_y));
        let color = color_to_float(quad.color);
        let top_left = TextVertex {
            position: [min_x, min_y],
            uv: [quad.uv.min_x, quad.uv.min_y],
            color,
        };
        let top_right = TextVertex {
            position: [max_x, min_y],
            uv: [quad.uv.max_x, quad.uv.min_y],
            color,
        };
        let bottom_right = TextVertex {
            position: [max_x, max_y],
            uv: [quad.uv.max_x, quad.uv.max_y],
            color,
        };
        let bottom_left = TextVertex {
            position: [min_x, max_y],
            uv: [quad.uv.min_x, quad.uv.max_y],
            color,
        };
        vertices.extend([
            top_left,
            bottom_left,
            bottom_right,
            top_left,
            bottom_right,
            top_right,
        ]);
    }
}

pub(super) fn image_vertices(
    surface: SurfaceRect,
    pixels_per_point: f32,
    image: &KittyImagePlacement,
) -> Option<[TextVertex; 6]> {
    let uv = super::image_upload::source_uv_rect(image)?;
    let destination = snap_rect_to_pixel_grid(image.destination, surface, pixels_per_point);
    let transform = SurfaceNdcTransform::new(surface);
    let min_x = transform.x(destination.min_x);
    let max_x = transform.x(destination.max_x);
    let min_y = transform.y(destination.min_y);
    let max_y = transform.y(destination.max_y);
    let color = [1.0, 1.0, 1.0, 1.0];
    let top_left = TextVertex {
        position: [min_x, min_y],
        uv: [uv.min_x, uv.min_y],
        color,
    };
    let top_right = TextVertex {
        position: [max_x, min_y],
        uv: [uv.max_x, uv.min_y],
        color,
    };
    let bottom_right = TextVertex {
        position: [max_x, max_y],
        uv: [uv.max_x, uv.max_y],
        color,
    };
    let bottom_left = TextVertex {
        position: [min_x, max_y],
        uv: [uv.min_x, uv.max_y],
        color,
    };
    Some([
        top_left,
        bottom_left,
        bottom_right,
        top_left,
        bottom_right,
        top_right,
    ])
}

// Round `value` to the nearest physical pixel, measuring from `origin` so the grid is anchored to
// the surface edge rather than logical zero. A non-positive or non-finite scale disables snapping.
fn snap_to_grid(value: f32, origin: f32, scale: f32) -> f32 {
    if !scale.is_finite() || scale <= 0.0 {
        return value;
    }
    origin + ((value - origin) * scale).round() / scale
}

fn snap_rect_to_pixel_grid(
    rect: SurfaceRect,
    surface: SurfaceRect,
    pixels_per_point: f32,
) -> SurfaceRect {
    let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let min_x = snap_to_grid(rect.min_x, surface.min_x, scale);
    let min_y = snap_to_grid(rect.min_y, surface.min_y, scale);
    let max_x = snap_to_grid(rect.max_x, surface.min_x, scale).max(min_x + 1.0 / scale);
    let max_y = snap_to_grid(rect.max_y, surface.min_y, scale).max(min_y + 1.0 / scale);
    SurfaceRect {
        min_x,
        min_y,
        max_x,
        max_y,
    }
}

#[derive(Clone, Copy)]
struct SurfaceNdcTransform {
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
}

impl SurfaceNdcTransform {
    fn new(surface: SurfaceRect) -> Self {
        let scale_x = 2.0 / surface.width().max(1.0);
        let scale_y = 2.0 / surface.height().max(1.0);
        Self {
            scale_x,
            scale_y,
            offset_x: -surface.min_x * scale_x - 1.0,
            offset_y: 1.0 + surface.min_y * scale_y,
        }
    }

    fn x(self, x: f32) -> f32 {
        x * self.scale_x + self.offset_x
    }

    fn y(self, y: f32) -> f32 {
        self.offset_y - y * self.scale_y
    }
}

pub(super) fn color_to_float(color: PlanColor) -> [f32; 4] {
    [
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        f32::from(color.a) / 255.0,
    ]
}

pub(super) fn vertex_bytes<T>(vertices: &[T]) -> &[u8] {
    // SAFETY: `BackgroundVertex` and `TextVertex` are `#[repr(C)]` aggregates composed only of
    // `f32` arrays. Reinterpreting their contiguous slice storage as bytes is valid for upload to
    // WGPU, and `u8` has alignment 1 so there is no stricter alignment requirement.
    unsafe {
        std::slice::from_raw_parts(
            vertices.as_ptr().cast::<u8>(),
            std::mem::size_of_val(vertices),
        )
    }
}
