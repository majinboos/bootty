use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use super::super::*;
const ONE_PIXEL_PNG_BASE64: &str = concat!(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA",
    "DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
);
const ONE_PIXEL_PNG_APC: &str = concat!(
    "\x1b_Ga=T,f=100,q=1,i=31,p=1;",
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA",
    "DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
    "\x1b\\"
);

fn test_terminal_engine() -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    })
}

fn captured_pty_engine() -> Result<(TerminalEngine, Arc<Mutex<Vec<u8>>>)> {
    let mut engine = test_terminal_engine()?;
    let output = Arc::new(Mutex::new(Vec::new()));
    let capture = output.clone();
    engine.on_pty_write(move |_terminal, bytes| {
        capture
            .lock()
            .expect("pty output lock")
            .extend_from_slice(bytes);
    })?;
    Ok((engine, output))
}

fn base64_encode_ascii(input: &str) -> String {
    base64_encode_bytes(input.as_bytes())
}

fn base64_encode_bytes(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

fn raw_rgba_command(image_id: u32, placement_id: u32, width: u32, height: u32) -> String {
    let bytes = vec![0xff; width as usize * height as usize * 4];
    format!(
        "\x1b_Ga=T,t=d,i={image_id},p={placement_id},s={width},v={height};{}\x1b\\",
        base64_encode_bytes(&bytes)
    )
}

fn raw_rgb_command_dimensions_with_options(
    image_id: u32,
    placement_id: u32,
    width: u32,
    height: u32,
    options: &str,
) -> String {
    format!(
        "\x1b_Ga=T,t=d,f=24,i={image_id},p={placement_id},s={width},v={height},{options};{}\x1b\\",
        base64_encode_bytes(&vec![0xff; width as usize * height as usize * 3])
    )
}

fn unicode_placeholder_row(width: usize) -> String {
    std::iter::repeat_n('\u{10EEEE}', width).collect()
}

fn unicode_placeholder_cell(row: usize, col: usize) -> String {
    const FIRST_DIACRITICS: [char; 25] = [
        '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}',
        '\u{033F}', '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}',
        '\u{0352}', '\u{0357}', '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}',
        '\u{0367}', '\u{0368}', '\u{0369}', '\u{036A}',
    ];
    let mut cell = String::new();
    cell.push('\u{10EEEE}');
    cell.push(FIRST_DIACRITICS[row]);
    cell.push(FIRST_DIACRITICS[col]);
    cell
}
fn unicode_placeholder_grid_row(row: usize, width: usize) -> String {
    (0..width)
        .map(|col| unicode_placeholder_cell(row, col))
        .collect()
}

fn raw_rgb_transmit_command(image_id: u32, width: u32, height: u32) -> String {
    let bytes = vec![0xee; width as usize * height as usize * 3];
    format!(
        "\x1b_Ga=t,t=d,f=24,i={image_id},s={width},v={height};{}\x1b\\",
        base64_encode_bytes(&bytes)
    )
}
fn tmux_wrap(payload: &[u8]) -> Vec<u8> {
    let mut wrapped = b"\x1bPtmux;".to_vec();
    for byte in payload {
        if *byte == 0x1b {
            wrapped.push(0x1b);
        }
        wrapped.push(*byte);
    }
    wrapped.extend_from_slice(b"\x1b\\");
    wrapped
}

fn raw_kitty_terminal() -> libghostty_vt::ffi::Terminal {
    let mut terminal: libghostty_vt::ffi::Terminal = std::ptr::null_mut();
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_terminal_new(std::ptr::null(), &mut terminal, 10, 4),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        let scrollback_limit = 0_usize;
        assert_eq!(
            libghostty_vt::ffi::ghostty_terminal_set(
                terminal,
                libghostty_vt::ffi::TerminalOption::SCROLLBACK_MAX_BYTES,
                (&scrollback_limit as *const usize).cast(),
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_terminal_resize(terminal, 10, 4, 10, 20),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        let storage_limit = 64_u64 * 1024 * 1024;
        assert_eq!(
            libghostty_vt::ffi::ghostty_terminal_set(
                terminal,
                libghostty_vt::ffi::TerminalOption::KITTY_IMAGE_STORAGE_LIMIT,
                (&storage_limit as *const u64).cast(),
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
    }
    terminal
}

struct TempFixture {
    path: PathBuf,
}

impl TempFixture {}

impl AsRef<std::path::Path> for TempFixture {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        _ = std::fs::remove_file(&self.path);
    }
}

fn write_temp_fixture(name: &str, bytes: &[u8]) -> Result<TempFixture> {
    // Test thread names contain "::", which Windows forbids in filenames.
    let thread = std::thread::current()
        .name()
        .unwrap_or("test")
        .replace("::", "-");
    let path = std::env::temp_dir().join(format!("bootty-{name}-{}-{thread}", std::process::id()));
    write_fixture_at(path, bytes)
}

fn write_fixture_at(path: PathBuf, bytes: &[u8]) -> Result<TempFixture> {
    std::fs::write(&path, bytes)?;
    // Ghostty's kitty temp-dir check prefix-matches against the TMP/TEMP env
    // value, so keep the env-form path on Windows; canonicalize would turn it
    // into a `\\?\` long-form path that never matches. Unix still needs
    // canonicalization for symlinked temp dirs such as macOS /tmp.
    let path = if cfg!(windows) {
        path
    } else {
        path.canonicalize()?
    };
    Ok(TempFixture { path })
}

fn write_kitty_temporary_fixture(bytes: &[u8]) -> Result<TempFixture> {
    let thread = std::thread::current()
        .name()
        .unwrap_or("test")
        .replace("::", "-");
    let path = std::env::temp_dir().join(format!(
        "tty-graphics-protocol-bootty-{}-{thread}.png",
        std::process::id()
    ));
    write_fixture_at(path, bytes)
}

fn file_payload(path: impl AsRef<std::path::Path>) -> Result<String> {
    Ok(base64_encode_ascii(
        path.as_ref().to_str().context("non-UTF-8 temp path")?,
    ))
}

fn storage_test_engine() -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols: 100,
        rows: 100,
        cell_width: 1,
        cell_height: 1,
    })
}

fn image_placement_ids(frame: &RenderFrame) -> Vec<(u32, u32)> {
    let mut ids = frame
        .images
        .placements
        .iter()
        .map(|placement| (placement.image_id, placement.placement_id))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn base64_decode_ascii(input: &str) -> Result<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        anyhow::bail!("invalid base64 length");
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);

    for chunk in bytes.chunks(4) {
        let v0 = value(chunk[0]).context("invalid base64 byte")?;
        let v1 = value(chunk[1]).context("invalid base64 byte")?;
        let pad2 = chunk[2] == b'=';
        let pad3 = chunk[3] == b'=';
        let v2 = if pad2 {
            0
        } else {
            value(chunk[2]).context("invalid base64 byte")?
        };
        let v3 = if pad3 {
            0
        } else {
            value(chunk[3]).context("invalid base64 byte")?
        };

        out.push((v0 << 2) | (v1 >> 4));
        if !pad2 {
            out.push(((v1 & 0b0000_1111) << 4) | (v2 >> 2));
        }
        if !pad3 {
            out.push(((v2 & 0b0000_0011) << 6) | v3);
        }
    }

    Ok(out)
}

fn lock_pty_output(output: &Arc<Mutex<Vec<u8>>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
    output.lock().expect("pty output lock")
}

fn assert_pty_output_empty(output: &Arc<Mutex<Vec<u8>>>) {
    assert!(lock_pty_output(output).is_empty());
}

fn assert_kitty_response(output: &Arc<Mutex<Vec<u8>>>, image_id: u32, status: &str) {
    assert_eq!(
        lock_pty_output(output).as_slice(),
        format!("\x1b_Gi={image_id};{status}\x1b\\").as_bytes()
    );
}

#[test]
fn terminal_engine_reports_ghostty_compatible_xtversion() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b[>q");
    let output = lock_pty_output(&output);

    assert!(
        output
            .windows(b"ghostty".len())
            .any(|window| window == b"ghostty"),
        "XTVERSION should advertise Ghostty compatibility: {:?}",
        String::from_utf8_lossy(&output),
    );
    assert!(
        output
            .windows(b"Bootty".len())
            .any(|window| window == b"Bootty"),
        "XTVERSION should preserve Bootty branding: {:?}",
        String::from_utf8_lossy(&output),
    );
    Ok(())
}

#[test]
fn terminal_engine_reports_cell_size_for_timg_queries() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b[16t");

    assert_eq!(lock_pty_output(&output).as_slice(), b"\x1b[6;16;8t");
    Ok(())
}

#[test]
fn terminal_engine_reports_physical_cell_size_for_timg_queries() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    engine.set_display_scale(2.0);

    engine.write_vt(b"\x1b[16t");

    assert_eq!(lock_pty_output(&output).as_slice(), b"\x1b[6;32;16t");
    Ok(())
}

#[test]
fn terminal_engine_reports_physical_render_cell_size_for_timg_queries() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;
    engine.set_display_scale(2.0);
    engine.set_render_cell_metrics(CellMetrics::new(8.4, 17.8));

    engine.write_vt(b"\x1b[16t");

    assert_eq!(lock_pty_output(&output).as_slice(), b"\x1b[6;36;17t");
    Ok(())
}

#[test]
fn terminal_engine_decodes_kitty_png_payloads_into_image_frame() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(
        b"\x1b_Ga=T,f=100,q=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA\
          DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\",
    );
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 1);
    Ok(())
}

#[test]
fn terminal_engine_direct_kitty_image_uses_full_intrinsic_height() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    })?;
    engine.write_vt(raw_rgb_command_dimensions_with_options(90, 1, 400, 66, "q=1").as_bytes());

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 90)
        .context("direct image placement")?;

    assert_eq!(placement.source.y, 0);
    assert_eq!(placement.source.height, 66);
    assert_eq!(placement.destination.height(), 66.0);
    Ok(())
}

#[test]
fn terminal_engine_direct_kitty_image_scales_intrinsic_pixels_to_logical_points() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    })?;
    engine.set_display_scale(2.0);
    engine.write_vt(raw_rgb_command_dimensions_with_options(91, 1, 30, 40, "q=1").as_bytes());

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 91)
        .context("direct image placement")?;

    assert_eq!(placement.destination.width(), 15.0);
    assert_eq!(placement.destination.height(), 20.0);
    Ok(())
}

#[test]
fn terminal_engine_decodes_split_prefix_kitty_png_payload() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let split_at = 2;

    engine.write_vt(&ONE_PIXEL_PNG_APC.as_bytes()[..split_at]);
    assert!(engine.extract_frame()?.images.placements.is_empty());

    engine.write_vt(&ONE_PIXEL_PNG_APC.as_bytes()[split_at..]);
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 1);
    Ok(())
}

#[test]
fn terminal_engine_loads_kitty_png_from_regular_file() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let path = write_temp_fixture(
        "kitty-file-image.png",
        &base64_decode_ascii(ONE_PIXEL_PNG_BASE64)?,
    )?;
    let command = format!("\x1b_Ga=T,f=100,t=f,q=1;{}\x1b\\", file_payload(&path)?);

    engine.write_vt(command.as_bytes());
    let frame = engine.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 1);
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_image_png_file_and_media_limits() -> Result<()> {
    let png_path = write_temp_fixture(
        "tty-graphics-protocol-image.png",
        &base64_decode_ascii(ONE_PIXEL_PNG_BASE64)?,
    )?;
    let mut png = test_terminal_engine()?;
    png.write_vt(
        format!(
            "\x1b_Ga=T,f=100,t=f,i=70,q=1;{}\x1b\\",
            file_payload(&png_path)?
        )
        .as_bytes(),
    );
    let frame = png.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_id, 70);
    assert_eq!(
        frame.images.placements[0].image_format,
        libghostty_vt::kitty::graphics::ImageFormat::Rgba
    );
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 1);

    let (mut shared_memory, shared_memory_output) = captured_pty_engine()?;
    shared_memory.write_vt(b"\x1b_Ga=t,f=24,t=s,i=71,s=1,v=1;c2htLW5hbWU=\x1b\\");
    assert_kitty_response(&shared_memory_output, 71, "EINVAL: invalid data");
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_command_long_value_compatibility() {
    let (mut long_value, long_value_output) = captured_pty_engine().expect("captured pty engine");
    long_value
        .write_vt(b"\x1b_Ga=t,f=24,s=10,v=2000000000000000000000000000000000000000,i=75\x1b\\");
    assert_kitty_response(&long_value_output, 75, "EINVAL: dimensions required");
}

#[test]
fn terminal_engine_ports_kitty_command_parser_edge_cases() -> Result<()> {
    let mut negative_i32 = test_terminal_engine()?;
    negative_i32.write_vt(b"\x1b_Ga=T,t=d,f=24,i=76,s=1,v=1,q=1;////\x1b\\");
    negative_i32.write_vt(b"\x1b_Ga=p,U=1,i=76,p=1,c=1,r=1,z=-2000000000\x1b\\");
    negative_i32.write_vt("\u{10EEEE}".as_bytes());
    assert!(
        negative_i32
            .extract_frame()?
            .images
            .virtual_placements
            .iter()
            .any(|placement| placement.image_id == 76 && placement.z == -2_000_000_000)
    );

    for input in [
        b"\x1b_Ga=p,i=10000000000\x1b\\".as_slice(),
        b"\x1b_Ga=p,i=1,z=-9999999999\x1b\\",
        b"\x1b_G;AAAA\x1b\\",
    ] {
        let (mut terminal, output) = captured_pty_engine()?;
        terminal.write_vt(input);
        assert_pty_output_empty(&output);
    }
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_delete_all_images_command() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(ONE_PIXEL_PNG_APC.as_bytes());
    let frame = engine.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);

    engine.write_vt(b"\x1b_Ga=d,d=A\x1b\\");
    let frame = engine.extract_frame()?;
    assert!(frame.images.placements.is_empty());

    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_storage_zero_placement_ids() -> Result<()> {
    let mut engine = storage_test_engine()?;

    engine.write_vt(raw_rgba_command(1, 0, 1, 1).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,i=1,p=0,c=1,r=1,q=1\x1b\\");
    let frame = engine.extract_frame()?;

    assert_eq!(image_placement_ids(frame), [(1, 0), (1, 1)]);
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_storage_delete_by_cursor_column_and_row() -> Result<()> {
    let mut engine = storage_test_engine()?;

    engine.write_vt(b"\x1b[1;1H");
    engine.write_vt(raw_rgba_command(1, 1, 50, 50).as_bytes());
    engine.write_vt(b"\x1b[26;26H");
    engine.write_vt(b"\x1b_Ga=p,i=1,p=2,q=1\x1b\\");
    assert_eq!(
        image_placement_ids(engine.extract_frame()?),
        [(1, 1), (1, 2)]
    );

    engine.write_vt(b"\x1b[13;13H\x1b_Ga=d,d=c\x1b\\");
    assert_eq!(image_placement_ids(engine.extract_frame()?), [(1, 2)]);

    engine.write_vt(b"\x1b_Ga=d,d=a\x1b\\");
    engine.write_vt(b"\x1b[1;1H");
    engine.write_vt(raw_rgba_command(1, 1, 50, 50).as_bytes());
    engine.write_vt(b"\x1b[26;26H");
    engine.write_vt(b"\x1b_Ga=p,i=1,p=2,q=1\x1b\\");
    engine.write_vt(b"\x1b_Ga=d,d=x,x=60\x1b\\");
    assert_eq!(image_placement_ids(engine.extract_frame()?), [(1, 1)]);

    engine.write_vt(b"\x1b_Ga=d,d=a\x1b\\");
    engine.write_vt(b"\x1b[1;1H");
    engine.write_vt(raw_rgba_command(1, 1, 50, 50).as_bytes());
    engine.write_vt(b"\x1b[26;26H");
    engine.write_vt(b"\x1b_Ga=p,i=1,p=2,q=1\x1b\\");
    engine.write_vt(b"\x1b_Ga=d,d=y,y=60\x1b\\");
    assert_eq!(image_placement_ids(engine.extract_frame()?), [(1, 1)]);

    engine.write_vt(b"\x1b_Ga=d,d=a\x1b\\");
    for column in 0..3 {
        engine.write_vt(format!("\x1b[1;{}H", column + 1).as_bytes());
        engine.write_vt(raw_rgba_command(1, column + 1, 1, 1).as_bytes());
    }
    engine.write_vt(b"\x1b_Ga=d,d=x,x=2\x1b\\");
    assert_eq!(
        image_placement_ids(engine.extract_frame()?),
        [(1, 1), (1, 3)]
    );

    engine.write_vt(b"\x1b_Ga=d,d=a\x1b\\");
    for row in 0..3 {
        engine.write_vt(format!("\x1b[{};1H", row + 1).as_bytes());
        engine.write_vt(raw_rgba_command(1, row + 1, 1, 1).as_bytes());
    }
    engine.write_vt(b"\x1b_Ga=d,d=y,y=2\x1b\\");
    assert_eq!(
        image_placement_ids(engine.extract_frame()?),
        [(1, 1), (1, 3)]
    );
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_storage_single_axis_aspect_ratio() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 100,
        rows: 100,
        cell_width: 10,
        cell_height: 20,
    })?;
    let bytes = vec![0xff; 16 * 9 * 4];
    let payload = base64_encode_bytes(&bytes);

    engine.write_vt(format!("\x1b_Ga=T,t=d,i=1,p=1,s=16,v=9,c=10;{payload}\x1b\\").as_bytes());
    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 1)
        .expect("column-sized placement");
    assert_eq!(placement.destination.width(), 100.0);
    assert_eq!(placement.destination.height(), 56.0);

    engine.write_vt(b"\x1b_Ga=d,d=A\x1b\\");
    engine.write_vt(format!("\x1b_Ga=T,t=d,i=2,p=1,s=16,v=9,r=5;{payload}\x1b\\").as_bytes());
    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 2)
        .expect("row-sized placement");
    assert_eq!(placement.destination.width(), 178.0);
    assert_eq!(placement.destination.height(), 100.0);
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_chunk_response_policy() -> Result<()> {
    let (mut quiet, quiet_output) = captured_pty_engine()?;
    quiet.write_vt(b"\x1b_Ga=T,f=24,t=d,i=1,s=1,v=2,c=10,r=1,m=1,q=1;////\x1b\\");
    quiet.write_vt(b"\x1b_Gm=0;////\x1b\\");
    assert_pty_output_empty(&quiet_output);

    let (mut responding, responding_output) = captured_pty_engine()?;
    responding.write_vt(b"\x1b_Ga=t,f=24,t=d,i=1,s=1,v=2,c=10,r=1,m=1,q=0;////\x1b\\");
    responding.write_vt(b"\x1b_Gm=0;////\x1b\\");
    assert_kitty_response(&responding_output, 1, "OK");

    let (mut raised_quiet, raised_output) = captured_pty_engine()?;
    raised_quiet.write_vt(b"\x1b_Ga=t,f=24,t=d,i=1,s=1,v=2,c=10,r=1,m=1,q=0;////\x1b\\");
    raised_quiet.write_vt(b"\x1b_Gm=0,q=1;////\x1b\\");
    assert_pty_output_empty(&raised_output);

    Ok(())
}

#[test]
fn kitty_graphics_c_adapter_ports_multi_and_error_cases() {
    let terminal = raw_kitty_terminal();

    let payload = base64_encode_bytes(&[0xcc; 4 * 3 * 3]);
    let command = format!("\x1b_Ga=T,t=d,f=24,i=84,p=1,s=4,v=3,c=5,r=2;{payload}\x1b\\");
    unsafe {
        libghostty_vt::ffi::ghostty_terminal_vt_write(terminal, command.as_ptr(), command.len());
    }

    let mut graphics: libghostty_vt::ffi::KittyGraphics = std::ptr::null_mut();
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_terminal_get(
                terminal,
                libghostty_vt::ffi::TerminalData::KITTY_GRAPHICS,
                (&mut graphics as *mut libghostty_vt::ffi::KittyGraphics).cast(),
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
    }

    let image = unsafe { libghostty_vt::ffi::ghostty_kitty_graphics_image(graphics, 84) };
    assert!(!image.is_null());
    assert!(unsafe { libghostty_vt::ffi::ghostty_kitty_graphics_image(graphics, 999) }.is_null());

    let mut image_id = 0_u32;
    let mut width = 0_u32;
    let mut data_len = 0_usize;
    let image_keys = [
        libghostty_vt::ffi::KittyGraphicsImageData::ID,
        libghostty_vt::ffi::KittyGraphicsImageData::WIDTH,
        libghostty_vt::ffi::KittyGraphicsImageData::DATA_LEN,
    ];
    let mut image_values: [*mut std::ffi::c_void; 3] = [
        (&mut image_id as *mut u32).cast(),
        (&mut width as *mut u32).cast(),
        (&mut data_len as *mut usize).cast(),
    ];
    let mut written = 0_usize;
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_image_get_multi(
                image,
                image_keys.len(),
                image_keys.as_ptr(),
                image_values.as_mut_ptr(),
                &mut written,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
    }
    assert_eq!((written, image_id, width, data_len), (3, 84, 4, 4 * 3 * 3));

    let invalid_image_keys = [
        libghostty_vt::ffi::KittyGraphicsImageData::ID,
        libghostty_vt::ffi::KittyGraphicsImageData::INVALID,
    ];
    written = usize::MAX;
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_image_get_multi(
                image,
                invalid_image_keys.len(),
                invalid_image_keys.as_ptr(),
                image_values.as_mut_ptr(),
                &mut written,
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_image_get_multi(
                image,
                1,
                std::ptr::null(),
                image_values.as_mut_ptr(),
                std::ptr::null_mut(),
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_image_get(
                std::ptr::null(),
                libghostty_vt::ffi::KittyGraphicsImageData::ID,
                (&mut image_id as *mut u32).cast(),
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
    }
    assert_eq!(written, 1);

    let mut iter: libghostty_vt::ffi::KittyGraphicsPlacementIterator = std::ptr::null_mut();
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_iterator_new(
                std::ptr::null(),
                &mut iter,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_get(
                iter,
                libghostty_vt::ffi::KittyGraphicsPlacementData::IMAGE_ID,
                (&mut image_id as *mut u32).cast(),
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_get(
                graphics,
                libghostty_vt::ffi::KittyGraphicsData::PLACEMENT_ITERATOR,
                (&mut iter as *mut libghostty_vt::ffi::KittyGraphicsPlacementIterator).cast(),
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert!(libghostty_vt::ffi::ghostty_kitty_graphics_placement_next(
            iter
        ));
    }

    let mut placement_id = 0_u32;
    let mut columns = 0_u32;
    let mut rows = 0_u32;
    let placement_keys = [
        libghostty_vt::ffi::KittyGraphicsPlacementData::PLACEMENT_ID,
        libghostty_vt::ffi::KittyGraphicsPlacementData::COLUMNS,
        libghostty_vt::ffi::KittyGraphicsPlacementData::ROWS,
    ];
    let mut placement_values: [*mut std::ffi::c_void; 3] = [
        (&mut placement_id as *mut u32).cast(),
        (&mut columns as *mut u32).cast(),
        (&mut rows as *mut u32).cast(),
    ];
    written = 0;
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_get_multi(
                iter,
                placement_keys.len(),
                placement_keys.as_ptr(),
                placement_values.as_mut_ptr(),
                &mut written,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
    }
    assert_eq!((written, placement_id, columns, rows), (3, 1, 5, 2));

    let mut pixel_width = 0_u32;
    let mut pixel_height = 0_u32;
    let mut grid_cols = 0_u32;
    let mut grid_rows = 0_u32;
    let mut viewport_col = 0_i32;
    let mut viewport_row = 0_i32;
    let mut source_x = 0_u32;
    let mut source_y = 0_u32;
    let mut source_width = 0_u32;
    let mut source_height = 0_u32;
    let mut render_info = libghostty_vt::ffi::KittyGraphicsPlacementRenderInfo {
        size: std::mem::size_of::<libghostty_vt::ffi::KittyGraphicsPlacementRenderInfo>(),
        ..Default::default()
    };
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_pixel_size(
                iter,
                image,
                terminal,
                &mut pixel_width,
                &mut pixel_height,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_grid_size(
                iter,
                image,
                terminal,
                &mut grid_cols,
                &mut grid_rows,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_viewport_pos(
                iter,
                image,
                terminal,
                &mut viewport_col,
                &mut viewport_row,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_source_rect(
                iter,
                image,
                &mut source_x,
                &mut source_y,
                &mut source_width,
                &mut source_height,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_render_info(
                iter,
                image,
                terminal,
                &mut render_info,
            ),
            libghostty_vt::ffi::Result::SUCCESS,
        );
    }
    assert_eq!((pixel_width, pixel_height), (50, 40));
    assert_eq!((grid_cols, grid_rows), (5, 2));
    assert_eq!((viewport_col, viewport_row), (0, 0));
    assert_eq!(
        (source_x, source_y, source_width, source_height),
        (0, 0, 4, 3)
    );
    assert!(render_info.viewport_visible);

    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_get_multi(
                iter,
                1,
                std::ptr::null(),
                placement_values.as_mut_ptr(),
                std::ptr::null_mut(),
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_pixel_size(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut pixel_width,
                &mut pixel_height,
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_grid_size(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut grid_cols,
                &mut grid_rows,
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_viewport_pos(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut viewport_col,
                &mut viewport_row,
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_source_rect(
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut source_x,
                &mut source_y,
                &mut source_width,
                &mut source_height,
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        assert_eq!(
            libghostty_vt::ffi::ghostty_kitty_graphics_placement_render_info(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut render_info,
            ),
            libghostty_vt::ffi::Result::INVALID_VALUE,
        );
        libghostty_vt::ffi::ghostty_kitty_graphics_placement_iterator_free(iter);
        libghostty_vt::ffi::ghostty_kitty_graphics_placement_iterator_free(std::ptr::null_mut());
        libghostty_vt::ffi::ghostty_terminal_free(terminal);
    }
}

#[test]
fn terminal_engine_ports_kitty_error_responses_for_valid_identifier_extremes() -> Result<()> {
    for (command, image_id) in [
        (
            b"\x1b_Ga=p,i=4294967295\x1b\\".as_slice(),
            4_294_967_295_u32,
        ),
        (b"\x1b_Ga=p,i=1,z=-2147483648\x1b\\".as_slice(), 1_u32),
    ] {
        let (mut terminal, output) = captured_pty_engine()?;
        terminal.write_vt(command);
        assert_kitty_response(&output, image_id, "ENOENT: image not found");
    }
    Ok(())
}

#[test]
fn terminal_engine_suppresses_kitty_response_without_image_id_or_number() -> Result<()> {
    let (mut transmit, transmit_output) = captured_pty_engine()?;
    transmit.write_vt(b"\x1b_Ga=t,f=24,t=d,s=1,v=2,c=10,r=1,i=0,I=0;////////\x1b\\");
    assert_pty_output_empty(&transmit_output);

    let (mut transmit_display, transmit_display_output) = captured_pty_engine()?;
    transmit_display.write_vt(b"\x1b_Ga=T,f=24,t=d,s=1,v=2,c=10,r=1,i=0,I=0;////////\x1b\\");
    assert_pty_output_empty(&transmit_display_output);
    Ok(())
}

#[test]
fn terminal_engine_exposes_kitty_virtual_placement_metadata() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(b"\x1b_Ga=T,t=d,f=24,i=31,s=1,v=1,q=1;////\x1b\\");
    engine.write_vt(b"\x1b_Ga=p,U=1,i=31,p=7,c=2,r=1,q=1\x1b\\");
    engine.write_vt("\x1b[38;5;31m\u{10EEEE}\x1b[39m".as_bytes());
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.virtual_placements.len(), 1);
    let placement = frame.images.virtual_placements[0];
    assert_eq!(placement.image_id, 31);
    assert_eq!(placement.placement_id, 7);
    assert_eq!(placement.columns, 2);
    assert_eq!(placement.rows, 1);
    assert_eq!(frame.images.virtual_placeholder_rows, vec![0]);
    Ok(())
}

#[test]
fn terminal_engine_resolves_palette_colored_virtual_placeholder_when_storage_is_unique()
-> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 3,
        cell_width: 10,
        cell_height: 20,
    })?;
    let image_id = 525_626_113;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(image_id, 0, 10, 20, "U=1,c=1,r=1,q=1").as_bytes(),
    );
    engine.write_vt("\x1b[38;5;70m\u{10EEEE}\x1b[39m".as_bytes());
    let frame = engine.extract_frame()?;

    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == image_id),
        "unique virtual storage placement should tolerate palette-colored placeholder ids: {:?}",
        frame.images.placements
    );
    Ok(())
}

#[test]
fn terminal_engine_reports_only_rows_with_actual_virtual_placeholder_cells() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 3,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(raw_rgb_transmit_command(93, 10, 20).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=93,c=1,r=1,q=1\x1b\\");
    engine.write_vt("\x1b[38;5;93m\u{10EEEE}\x1b[39m\nEND\n>".as_bytes());
    let frame = engine.extract_frame()?;

    for row in &frame.images.virtual_placeholder_rows {
        assert!(
            frame
                .images
                .placements
                .iter()
                .any(|placement| placement.destination.min_y == f32::from(*row) * 20.0),
            "virtual placeholder row {row} must have an actual placement"
        );
    }

    Ok(())
}

#[test]
fn terminal_engine_keeps_timg_sized_virtual_image_out_of_following_text_row() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 213,
        rows: 51,
        cell_width: 8,
        cell_height: 16,
    })?;
    let image_id = 94;
    let placeholder_row = unicode_placeholder_row(121);

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(image_id, 1, 121, 98, "U=1,c=121,r=49,q=1")
            .as_bytes(),
    );
    engine.write_vt(b"\x1b[38;5;94m");
    for _ in 0..49 {
        engine.write_vt(placeholder_row.as_bytes());
        engine.write_vt(b"\r\n");
    }
    engine.write_vt(b"\x1b[39mEND_MARKER\r\n");
    let frame = engine.extract_frame()?;

    let marker_row_top = 49.0 * 16.0;
    let placements = frame
        .images
        .placements
        .iter()
        .filter(|placement| placement.image_id == image_id)
        .collect::<Vec<_>>();
    assert!(!placements.is_empty());
    assert!(
        placements
            .iter()
            .all(|placement| placement.destination.max_y <= marker_row_top),
        "virtual image placement leaked into marker row: {:?}",
        placements
            .iter()
            .map(|placement| placement.destination)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        frame
            .cells
            .iter()
            .find(|cell| frame.cell_text(cell).starts_with(&['E']))
            .map(|cell| cell.y),
        Some(49)
    );

    Ok(())
}

#[test]
fn terminal_engine_keeps_real_timg_tmux_canvas_out_of_two_line_prompt() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 213,
        rows: 52,
        cell_width: 7,
        cell_height: 23,
    })?;
    let image_id = 95;
    let placeholder_row = unicode_placeholder_row(123);

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(image_id, 1, 123, 164, "U=1,c=123,r=50,q=1")
            .as_bytes(),
    );
    engine.write_vt(b"\x1b[38;5;95m");
    for _ in 0..50 {
        engine.write_vt(placeholder_row.as_bytes());
        engine.write_vt(b"\r\n");
    }
    engine.write_vt(b"\x1b[39m~/Downloads\r\n\x1b[32m\xe2\x9d\xaf\x1b[39m");
    let frame = engine.extract_frame()?;

    let prompt_top = 50.0 * 23.0;
    let placements = frame
        .images
        .placements
        .iter()
        .filter(|placement| placement.image_id == image_id)
        .collect::<Vec<_>>();
    assert!(!placements.is_empty());
    assert!(
        placements
            .iter()
            .all(|placement| placement.destination.max_y <= prompt_top),
        "virtual image placement leaked into prompt rows: {:?}",
        placements
            .iter()
            .map(|placement| placement.destination)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        frame
            .cells
            .iter()
            .find(|cell| frame.cell_text(cell).starts_with(&['~']))
            .map(|cell| cell.y),
        Some(50)
    );
    assert_eq!(
        frame
            .cells
            .iter()
            .find(|cell| frame.cell_text(cell).starts_with(&['❯']))
            .map(|cell| cell.y),
        Some(51)
    );

    Ok(())
}

#[test]
fn terminal_engine_virtual_wide_image_slices_merge_to_full_source_height() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    })?;
    let image_id = 96;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(image_id, 1, 400, 66, "U=1,c=25,r=3,q=1")
            .as_bytes(),
    );
    engine.write_vt(b"\x1b[38;5;96m");
    for row in 0..3 {
        engine.write_vt(unicode_placeholder_grid_row(row, 25).as_bytes());
        engine.write_vt(b"\r\n");
    }

    let frame = engine.extract_frame()?;
    let placements = frame
        .images
        .placements
        .iter()
        .filter(|placement| placement.image_id == image_id)
        .collect::<Vec<_>>();
    let summary = placements
        .iter()
        .map(|placement| (placement.source, placement.destination))
        .collect::<Vec<_>>();
    assert_eq!(placements.len(), 1, "slices should merge: {summary:?}");
    assert_eq!(placements[0].source.y, 0);
    assert_eq!(placements[0].source.height, 66);
    assert_eq!(placements[0].destination.min_y, 0.0);
    assert_eq!(placements[0].destination.max_y, 60.0);
    Ok(())
}

#[test]
fn terminal_engine_merges_adjacent_virtual_image_rows() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 4,
        rows: 3,
        cell_width: 10,
        cell_height: 20,
    })?;

    let row0 = format!(
        "{}{}",
        unicode_placeholder_cell(0, 0),
        unicode_placeholder_cell(0, 1)
    );
    let row1 = format!(
        "{}{}",
        unicode_placeholder_cell(1, 0),
        unicode_placeholder_cell(1, 1)
    );
    engine.write_vt(raw_rgb_transmit_command(97, 20, 40).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=97,c=2,r=2,q=1\x1b\\");
    engine.write_vt(format!("\x1b[38;5;97m{row0}\r\n{row1}\x1b[39m").as_bytes());

    let frame = engine.extract_frame()?;
    let placements = frame
        .images
        .placements
        .iter()
        .filter(|placement| placement.image_id == 97)
        .collect::<Vec<_>>();
    assert_eq!(
        placements.len(),
        1,
        "adjacent rows should share one image placement"
    );
    assert_eq!(placements[0].destination.min_y, 0.0);
    assert_eq!(placements[0].destination.max_y, 40.0);

    Ok(())
}

#[test]
fn native_kitty_image_disappears_after_screen_clear() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(99, 1, 40, 60, "c=4,r=3,C=1,q=1").as_bytes(),
    );
    assert!(
        engine
            .extract_frame()?
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 99)
    );

    engine.write_vt(b"\x1b[H\x1b[2JAFTER_CLEAR");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 99),
        "native image should not survive a screen clear/redraw: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn native_kitty_image_survives_reserved_rows_before_first_frame() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(101, 1, 40, 40, "c=4,r=2,C=1,q=1").as_bytes(),
    );
    engine.write_vt(b"\r\n\r\n");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 101),
        "reserved rows that arrive before first paint must not hide the image: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn native_kitty_image_survives_preceding_command_text_and_reserved_rows() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 6,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(b"clear; show-image\r\n");
    engine.write_vt(
        raw_rgb_command_dimensions_with_options(104, 1, 40, 40, "c=4,r=2,C=1,q=1").as_bytes(),
    );
    engine.write_vt(b"\r\n\r\nPI_STYLE_DONE");
    let frame = engine.extract_frame()?;
    let placements = frame.images.placements.clone();
    assert!(
        placements.iter().any(|placement| placement.image_id == 104),
        "preceding shell text and reserved rows must not hide the image; placements={placements:?}",
    );

    Ok(())
}

#[test]
fn native_kitty_image_excludes_marker_after_declared_reserved_rows() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 40,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(105, 1, 600, 480, "c=60,r=24,C=1,q=1").as_bytes(),
    );
    engine.write_vt(b"\r\n".repeat(24).as_slice());
    engine.write_vt(b"PI_STYLE_DONE");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 105),
        "marker after declared reserved rows must not hide the image: {:?}",
        frame.images.placements
    );

    Ok(())
}
#[test]
fn native_kitty_image_survives_blank_reserved_rows_after_first_frame() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(102, 1, 40, 40, "c=4,r=2,C=1,q=1").as_bytes(),
    );
    assert!(
        engine
            .extract_frame()?
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 102)
    );

    engine.write_vt(b"\r\n\r\n");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 102),
        "blank reserved rows must not hide an already-painted image: {:?}",
        frame.images.placements
    );

    Ok(())
}
#[test]
fn native_kitty_image_reappears_after_temporary_text_overlap() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(100, 1, 40, 60, "c=4,r=3,C=1,q=1").as_bytes(),
    );
    assert!(
        engine
            .extract_frame()?
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 100)
    );

    engine.write_vt(b"\x1b[1;1Hcopy-mode\r\n------------\r\n------------");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 100),
        "native image should hide while text overlaps its declared rows: {:?}",
        frame.images.placements
    );

    engine.write_vt(b"\x1b[1;1H\x1b[J");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 100),
        "native image should reappear when its declared rows are blank again: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn native_kitty_image_survives_same_row_text_outside_declared_columns() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(101, 1, 40, 40, "c=4,r=2,C=1,q=1").as_bytes(),
    );
    assert!(
        engine
            .extract_frame()?
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 101)
    );

    engine.write_vt(b"\x1b[1;10HOK");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 101),
        "native image should stay visible when same-row text is outside its columns: {:?}",
        frame.images.placements
    );

    Ok(())
}
#[test]
fn native_kitty_image_tracks_scrollback_viewport_rows() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 12,
            rows: 4,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(98, 1, 40, 40, "c=4,r=2,C=1,q=1").as_bytes(),
    );
    engine.write_vt(b"\r\n\r\n\r\nrow3\r\nrow4\r\nrow5\r\nrow6");

    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 98),
        "image should start outside the bottom viewport: {:?}",
        frame.images.placements
    );

    engine.scroll_viewport_delta(-6);
    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 98)
        .expect("scrolled viewport should expose native Kitty image");
    assert_eq!(placement.destination.min_y, 0.0);
    assert_eq!(placement.destination.max_y, 40.0);

    engine.scroll_viewport_bottom();
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 98),
        "image should leave the viewport instead of staying screen-absolute: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn native_kitty_image_reappears_when_scrolled_back_to_reserved_rows() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 12,
            rows: 4,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(103, 1, 40, 40, "c=4,r=2,C=1,q=1").as_bytes(),
    );
    engine.write_vt(b"\r\n\r\n\r\nrow3");
    assert!(
        engine
            .extract_frame()?
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 103)
    );

    engine.write_vt(b"\r\nrow4\r\nrow5\r\nrow6");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 103),
        "image should leave the bottom viewport: {:?}",
        frame.images.placements
    );

    engine.scroll_viewport_delta(-6);
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 103),
        "scrolling back to the image rows should show it again: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn virtual_image_tracks_scrollback_viewport_rows() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 12,
            rows: 3,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;

    engine.write_vt(raw_rgb_transmit_command(96, 10, 20).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=96,c=1,r=1,q=1\x1b\\");
    engine.write_vt("\x1b[38;5;96m\u{10EEEE}\x1b[39m\r\nrow1\r\nrow2\r\nrow3".as_bytes());

    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 96),
        "image should start outside the bottom viewport: {:?}",
        frame.images.placements
    );

    engine.scroll_viewport_delta(-3);
    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 96)
        .expect("scrolled viewport should expose virtual image");
    assert_eq!(placement.destination.min_y, 0.0);

    engine.scroll_viewport_bottom();
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != 96),
        "image should leave the viewport instead of staying screen-absolute: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn tmux_style_virtual_image_transmit_tracks_scrollback_viewport_rows() -> Result<()> {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 12,
            rows: 3,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig::default(),
        NATIVE_MAX_SCROLLBACK,
    )?;
    let image_id = 97;
    let path = write_temp_fixture(
        "kitty-file-image.png",
        &base64_decode_ascii(ONE_PIXEL_PNG_BASE64)?,
    )?;
    let command = format!(
        "\x1b_Ga=T,t=f,f=100,U=1,i={image_id},c=2,r=2,q=1;{}\x1b\\",
        file_payload(&path)?,
    );
    let first_row = unicode_placeholder_grid_row(0, 2);
    let second_row = unicode_placeholder_grid_row(1, 2);

    engine.write_vt(command.as_bytes());
    engine.write_vt(format!("\x1b[38;2;0;0;{image_id}m{first_row}\x1b[39m\r\n").as_bytes());
    engine.write_vt(format!("\x1b[38;2;0;0;{image_id}m{second_row}\x1b[39m\r\n").as_bytes());
    engine.write_vt(b"row2\r\nrow3\r\nrow4");

    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != image_id),
        "image should start outside the bottom viewport: {:?}",
        frame.images.placements
    );

    engine.scroll_viewport_delta(-4);
    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == image_id)
        .expect("scrolled viewport should expose tmux-style virtual image");
    assert!(placement.destination.min_y >= 0.0);
    assert!(placement.destination.max_y <= 60.0);

    engine.scroll_viewport_bottom();
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .all(|placement| placement.image_id != image_id),
        "image should leave the viewport instead of staying screen-absolute: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn tmux_style_virtual_image_clears_when_placeholder_cells_are_removed() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 3,
        cell_width: 10,
        cell_height: 20,
    })?;
    let image_id = 99;
    let first_row = unicode_placeholder_grid_row(0, 2);
    let second_row = unicode_placeholder_grid_row(1, 2);

    engine.write_vt(
        raw_rgb_command_dimensions_with_options(image_id, 1, 20, 40, "U=1,c=2,r=2,q=1").as_bytes(),
    );
    engine.write_vt(format!("\x1b[38;2;0;0;{image_id}m{first_row}\x1b[39m\r\n").as_bytes());
    engine.write_vt(format!("\x1b[38;2;0;0;{image_id}m{second_row}\x1b[39m").as_bytes());
    assert!(
        engine
            .extract_frame()?
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == image_id)
    );

    engine.write_vt(b"\x1b[2J\x1b[Hnew-window");
    let frame = engine.extract_frame()?;
    assert!(
        frame.images.placements.is_empty(),
        "clearing placeholder cells must remove virtual image placements: {:?}",
        frame.images.placements
    );

    Ok(())
}

#[test]
fn virtual_image_reuses_cached_pixels_across_dirty_frames() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 12,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(raw_rgb_transmit_command(104, 20, 20).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=104,c=2,r=1,q=1\x1b\\");
    engine.write_vt("\x1b[38;5;104m\u{10EEEE}\u{10EEEE}\x1b[39m\r\n".as_bytes());
    let first = engine
        .extract_frame()?
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 104)
        .expect("virtual image placement")
        .data
        .clone();

    engine.write_vt(b"\x1b[4;1Hstatus");
    let second = engine
        .extract_frame()?
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 104)
        .expect("virtual image placement after unrelated redraw")
        .data
        .clone();

    assert!(
        Arc::ptr_eq(&first, &second),
        "virtual images should not re-copy/re-upload pixels on unrelated redraws"
    );

    Ok(())
}

#[test]
fn terminal_engine_virtual_image_infers_grid_from_logical_image_size() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;
    engine.set_display_scale(2.0);

    engine.write_vt(raw_rgb_transmit_command(105, 20, 40).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=105,q=1\x1b\\");
    engine.write_vt("\x1b[38;5;105m\u{10EEEE}\x1b[39m".as_bytes());

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 105)
        .expect("virtual image placement");

    assert_eq!(placement.source.x, 0);
    assert_eq!(placement.source.y, 0);
    assert_eq!(placement.source.width, 20);
    assert_eq!(placement.source.height, 40);
    assert_eq!(placement.destination.width(), 10.0);
    assert_eq!(placement.destination.height(), 20.0);

    Ok(())
}

#[test]
fn terminal_engine_virtual_image_uses_render_cell_metrics_for_destination() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 120,
        rows: 4,
        cell_width: 5,
        cell_height: 20,
    })?;
    engine.set_render_cell_metrics(CellMetrics::new(4.5, 20.0));

    engine.write_vt(raw_rgb_transmit_command(106, 18, 20).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=106,c=4,r=1,q=1\x1b\\");
    engine.write_vt(
        format!(
            "\x1b[101G\x1b[38;5;106m{}\x1b[39m",
            unicode_placeholder_row(4)
        )
        .as_bytes(),
    );

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 106)
        .expect("virtual image placement");

    assert_eq!(placement.destination.min_x, 450.0);
    assert_eq!(placement.destination.width(), 18.0);
    assert_eq!(placement.destination.height(), 20.0);

    Ok(())
}

#[test]
fn terminal_engine_virtual_cover_image_uses_full_grid_width_for_centered_rows() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 22,
    })?;
    engine.set_render_cell_metrics(CellMetrics::new(8.125, 22.3125));

    engine.write_vt(raw_rgb_transmit_command(107, 512, 512).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=107,c=22,r=8,q=1\x1b\\");
    engine.write_vt(b"\x1b[38;5;107m");
    for row in 0..8 {
        let line = (1..21)
            .map(|col| unicode_placeholder_cell(row, col))
            .collect::<String>();
        engine.write_vt(line.as_bytes());
        engine.write_vt(b"\r\n");
    }

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 107)
        .expect("centered virtual cover placement");

    assert_eq!(placement.source.x, 0);
    assert_eq!(placement.source.width, 512);
    assert_eq!(placement.destination.min_x, -8.125);
    assert_eq!(placement.destination.width(), 178.75);
    assert_eq!(placement.destination.height(), 178.75);

    Ok(())
}

#[test]
fn terminal_engine_virtual_square_image_keeps_full_grid_square() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 22,
    })?;
    engine.set_render_cell_metrics(CellMetrics::new(22.3125, 22.28125));

    engine.write_vt(raw_rgb_transmit_command(108, 512, 512).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=108,c=16,r=16,q=1\x1b\\");
    engine.write_vt(b"\x1b[38;5;108m");
    for row in 0..16 {
        let line = unicode_placeholder_grid_row(row, 16);
        engine.write_vt(line.as_bytes());
        engine.write_vt(b"\r\n");
    }

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 108)
        .expect("full-grid square virtual placement");

    assert_eq!(placement.source.width, 512);
    assert_eq!(placement.source.height, 512);
    assert_eq!(placement.destination.width(), 357.0);
    assert_eq!(placement.destination.height(), 357.0);

    Ok(())
}

#[test]
fn terminal_engine_virtual_square_icon_keeps_two_column_row_square() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 20,
    })?;

    engine.write_vt(raw_rgb_transmit_command(109, 24, 24).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=109,c=2,r=1,q=1\x1b\\");
    engine.write_vt(b"\x1b[38;5;109m");
    engine.write_vt(unicode_placeholder_grid_row(0, 2).as_bytes());

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 109)
        .expect("square icon virtual placement");

    assert_eq!(placement.source.width, 24);
    assert_eq!(placement.source.height, 24);
    assert_eq!(placement.destination.width(), 16.0);
    assert_eq!(placement.destination.min_y, 2.0);
    assert_eq!(placement.destination.height(), 16.0);

    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_unicode_placeholder_runs() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(raw_rgb_transmit_command(90, 40, 40).as_bytes());
    engine.write_vt(b"\x1b_Ga=p,U=1,i=90,c=4,r=2,q=1\x1b\\");
    engine.write_vt(
        "\x1b[38;5;90m\
         \u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\
         \u{10EEEE}\u{0305}\u{030E}\u{10EEEE}\u{0305}\u{0310}\n\
         \u{10EEEE}\u{030D}\u{0305}\u{10EEEE}\u{030D}\u{030D}\
         \u{10EEEE}\u{030D}\u{030E}\u{10EEEE}\u{030D}\u{0310}\x1b[39m"
            .as_bytes(),
    );

    let frame = engine.extract_frame()?;
    let mut image_placements = frame
        .images
        .placements
        .iter()
        .filter(|placement| placement.image_id == 90)
        .collect::<Vec<_>>();
    image_placements.sort_by_key(|placement| placement.source.y);

    assert_eq!(image_placements.len(), 2);
    assert_eq!(image_placements[0].source.x, 0);
    assert_eq!(image_placements[0].source.y, 0);
    assert_eq!(image_placements[0].source.width, 40);
    assert_eq!(image_placements[0].source.height, 20);
    assert_eq!(image_placements[0].destination.width(), 40.0);
    assert_eq!(image_placements[0].destination.height(), 20.0);
    assert_eq!(image_placements[1].source.x, 0);
    assert_eq!(image_placements[1].source.y, 20);
    assert_eq!(image_placements[1].source.width, 40);
    assert_eq!(image_placements[1].source.height, 20);
    assert!(
        frame
            .cells
            .iter()
            .filter(|cell| cell.y <= 1)
            .all(|cell| frame.cell_text(cell).is_empty())
    );
    assert_eq!(frame.images.virtual_placeholder_rows, vec![0, 1]);

    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_unicode_high_bits_and_placement_id() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let image_id = 33_554_474;

    engine.write_vt(raw_rgb_transmit_command(image_id, 1, 1).as_bytes());
    engine.write_vt(format!("\x1b_Ga=p,U=1,i={image_id},p=21,c=1,r=1,q=1\x1b\\").as_bytes());
    engine.write_vt(
        "\x1b[38;5;42m\x1b[58;5;21m\u{10EEEE}\u{0305}\u{0305}\u{030E}\x1b[39m\x1b[59m".as_bytes(),
    );

    let frame = engine.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == image_id && placement.placement_id == 21)
        .expect("high-bit unicode placement");

    assert_eq!(placement.source.width, 1);
    assert_eq!(placement.source.height, 1);
    assert_eq!(placement.destination.width(), 8.0);
    assert_eq!(placement.destination.height(), 16.0);

    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_unicode_continuation_edges() -> Result<()> {
    let mut continued = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 2,
        cell_width: 10,
        cell_height: 20,
    })?;
    continued.write_vt(raw_rgb_transmit_command(91, 100, 20).as_bytes());
    continued.write_vt(b"\x1b_Ga=p,U=1,i=91,c=10,r=1,q=1\x1b\\");
    continued.write_vt("\x1b[38;5;91m\u{10EEEE}\u{10EEEE}\u{10EEEE}\x1b[39m".as_bytes());
    let frame = continued.extract_frame()?;
    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == 91)
        .expect("continued placement");
    assert_eq!(placement.source.x, 0);
    assert_eq!(placement.source.width, 30);
    assert_eq!(placement.destination.width(), 30.0);

    let mut broken = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 2,
        cell_width: 10,
        cell_height: 20,
    })?;
    broken.write_vt(raw_rgb_transmit_command(92, 100, 20).as_bytes());
    broken.write_vt(b"\x1b_Ga=p,U=1,i=92,c=10,r=1,q=1\x1b\\");
    broken.write_vt(
        "\x1b[38;5;92m\
         \u{10EEEE}\u{0305}\u{0305}\
         \u{10EEEE}\u{0305}\u{030E}\x1b[39m"
            .as_bytes(),
    );
    let frame = broken.extract_frame()?;
    let mut placements = frame
        .images
        .placements
        .iter()
        .filter(|placement| placement.image_id == 92)
        .collect::<Vec<_>>();
    placements.sort_by_key(|placement| placement.source.x);
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[0].source.x, 0);
    assert_eq!(placements[0].source.width, 10);
    assert_eq!(placements[1].source.x, 20);
    assert_eq!(placements[1].source.width, 10);

    Ok(())
}

#[test]
fn terminal_engine_decodes_timg_style_kitty_png_payload_into_image_frame() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(
        b"\x1b[?25l\x1b_Ga=T,i=32024961,q=2,f=100,m=0;iVBORw0KGgoAAAANSUhEUgAAAAE\
          AAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\\x1b[?25h",
    );
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 1);
    Ok(())
}

#[test]
fn terminal_engine_decodes_chafa_style_empty_initial_rgba_chunk() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(b"\x1b_Ga=T,f=32,s=2,v=1,c=2,r=1,m=1,q=2\x1b\\");
    engine.write_vt(b"\x1b_Gm=1;////");
    engine.write_vt(b"//////8=\x1b\\");
    engine.write_vt(b"\x1b_Gm=0\x1b\\");
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_width, 2);
    assert_eq!(frame.images.placements[0].image_height, 1);
    Ok(())
}

#[test]
fn terminal_engine_decodes_tmux_passthrough_kitty_payloads() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(
        b"\x1bPtmux;\x1b\x1b_Ga=T,i=32024961,q=2,f=100,m=0;iVBORw0KGgoAAAANSUhEUgAAAAE\
          AAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\x1b\\\x1b\\",
    );
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 1);
    Ok(())
}

#[test]
fn terminal_engine_decodes_tmux_passthrough_chunked_kitty_payloads() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(&tmux_wrap(
        b"\x1b_Ga=T,f=24,t=d,i=86,s=1,v=2,m=1,q=1;////\x1b\\",
    ));
    assert!(engine.extract_frame()?.images.placements.is_empty());

    engine.write_vt(&tmux_wrap(b"\x1b_Gm=0,q=1;////\x1b\\"));
    let frame = engine.extract_frame()?;

    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_id, 86);
    assert_eq!(frame.images.placements[0].image_width, 1);
    assert_eq!(frame.images.placements[0].image_height, 2);
    Ok(())
}

#[test]
fn terminal_engine_decodes_timg_tmux_rgb_unicode_placeholder() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let image_id = 475_812_481;

    engine.write_vt(&tmux_wrap(
        b"\x1b_Ga=T,i=475812481,q=2,f=100,m=0,U=1,c=1,r=1;iVBORw0KGgoAAAANSUhEUgAAABQAAAAUCAYAAACNiR0NAAAAbUlEQVR4Aa3MgQDAIAAAwR/ABEYwgRFMYAIjmEAECUSQQAIRJBBBAhE0iT+A2xYsRNtkd8PB4Yad0w0blxtWbjcsPG6Yed0w8blhJLhhILrhR3LDl+yGD8UNb6obXjQ3POlueDDccGe6ISw1/AH8XifbYYnl/QAAAABJRU5ErkJggg==\x1b\\",
    ));
    engine.write_vt(
        "\r\x1b[38:2:92:82:129m\u{10EEEE}\u{0305}\u{0305}\u{036E}\x1b[39m\r\n".as_bytes(),
    );
    let frame = engine.extract_frame()?;

    let placement = frame
        .images
        .placements
        .iter()
        .find(|placement| placement.image_id == image_id)
        .expect("timg rgb placeholder placement");
    assert_eq!(placement.source.width, 20);
    assert_eq!(placement.source.height, 20);
    assert_eq!(placement.destination.min_y, 0.0);
    assert_eq!(placement.destination.height(), 16.0);
    assert!(
        placement.destination.max_y <= 16.0,
        "virtual placement must stay inside the placeholder row: {:?}",
        placement.destination
    );
    Ok(())
}

#[test]
fn terminal_engine_refreshes_reused_kitty_image_id_when_middle_bytes_change() -> Result<()> {
    let mut engine = test_terminal_engine()?;
    let first_bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8];
    let second_bytes = [0, 1, 2, 90, 91, 92, 6, 7, 8];

    engine.write_vt(
        format!(
            "\x1b_Ga=T,t=d,f=24,i=82,p=1,s=3,v=1;{}\x1b\\",
            base64_encode_bytes(&first_bytes)
        )
        .as_bytes(),
    );
    let first = engine.extract_frame()?.images.placements[0].data.clone();

    engine.write_vt(
        format!(
            "\x1b_Ga=T,t=d,f=24,i=82,p=1,s=3,v=1;{}\x1b\\",
            base64_encode_bytes(&second_bytes)
        )
        .as_bytes(),
    );
    let second = engine.extract_frame()?.images.placements[0].data.clone();

    assert_eq!(first.as_slice(), first_bytes);
    assert_eq!(second.as_slice(), second_bytes);
    assert!(!Arc::ptr_eq(&first, &second));
    Ok(())
}
