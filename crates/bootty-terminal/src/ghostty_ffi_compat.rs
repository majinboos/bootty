use std::{ffi::c_void, ptr};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

const GHOSTTY_NO_VALUE: i32 = -4;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ghostty_color_palette_generate_256(
    base: *const ColorRgb,
    skip: *const bool,
    bg: *const ColorRgb,
    fg: *const ColorRgb,
    _harmonious: bool,
    out: *mut ColorRgb,
) {
    if base.is_null() || out.is_null() {
        return;
    }

    for index in 0..256 {
        let keep = !skip.is_null() && unsafe { *skip.add(index) };
        let color = if keep || index < 16 {
            unsafe { *base.add(index) }
        } else {
            ansi_cube_color(index, bg, fg)
        };
        unsafe { ptr::write(out.add(index), color) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ghostty_osc_semantic_prompt_write_command_line(
    _command: *mut c_void,
    _out: *mut u8,
    _out_len: usize,
    out_written: *mut usize,
) -> i32 {
    if !out_written.is_null() {
        unsafe { ptr::write(out_written, 0) };
    }
    GHOSTTY_NO_VALUE
}

fn ansi_cube_color(index: usize, bg: *const ColorRgb, fg: *const ColorRgb) -> ColorRgb {
    if index == 16 && !bg.is_null() {
        return unsafe { *bg };
    }
    if index == 231 && !fg.is_null() {
        return unsafe { *fg };
    }
    if (16..=231).contains(&index) {
        let value = index - 16;
        let r = value / 36;
        let g = (value / 6) % 6;
        let b = value % 6;
        return ColorRgb {
            r: cube_component(r),
            g: cube_component(g),
            b: cube_component(b),
        };
    }
    if (232..=255).contains(&index) {
        let gray = 8 + ((index - 232) as u8) * 10;
        return ColorRgb {
            r: gray,
            g: gray,
            b: gray,
        };
    }
    ColorRgb::default()
}

fn cube_component(value: usize) -> u8 {
    if value == 0 { 0 } else { 55 + value as u8 * 40 }
}
