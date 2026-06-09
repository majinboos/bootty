use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use super::super::*;
use crate::terminal_png_decoder::png_frame_to_rgba8;

use libghostty_vt::kitty::graphics::{Layer, PlacementIterator};

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
const GHOSTTY_RGB_20X15: &[u8] = &[0x40; 20 * 15 * 3];
const GHOSTTY_RGB_ZLIB_128X96: &[u8] = &[0x20; 128 * 96 * 3];

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

fn raw_rgb_command_with_options(image_id: u32, placement_id: u32, options: &str) -> String {
    format!(
        "\x1b_Ga=T,t=d,f=24,i={image_id},p={placement_id},s=4,v=3,{options};{}\x1b\\",
        base64_encode_bytes(&[0xff; 4 * 3 * 3])
    )
}

fn raw_rgb_transmit_command(image_id: u32, width: u32, height: u32) -> String {
    let bytes = vec![0xee; width as usize * height as usize * 3];
    format!(
        "\x1b_Ga=t,t=d,f=24,i={image_id},s={width},v={height};{}\x1b\\",
        base64_encode_bytes(&bytes)
    )
}

fn raw_kitty_terminal() -> libghostty_vt::ffi::Terminal {
    let mut terminal: libghostty_vt::ffi::Terminal = std::ptr::null_mut();
    let options = libghostty_vt::ffi::TerminalOptions {
        cols: 10,
        rows: 4,
        max_scrollback: 0,
    };
    unsafe {
        assert_eq!(
            libghostty_vt::ffi::ghostty_terminal_new(std::ptr::null(), &mut terminal, options),
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

impl TempFixture {
    fn exists(&self) -> bool {
        self.path.exists()
    }
}

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
    let path = std::env::temp_dir().join(format!(
        "bootty-{name}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::write(&path, bytes)?;
    Ok(TempFixture {
        path: path.canonicalize()?,
    })
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
fn terminal_engine_advertises_and_accepts_kitty_file_images() -> Result<()> {
    let engine = test_terminal_engine()?;

    assert_eq!(TERMINAL_TERM, "xterm-ghostty");
    assert!(engine.terminal.kitty_image_storage_limit()? > 0);
    assert!(engine.terminal.is_kitty_image_from_file_allowed()?);
    assert!(engine.terminal.is_kitty_image_from_temp_file_allowed()?);
    assert!(!engine.terminal.is_kitty_image_from_shared_mem_allowed()?);
    Ok(())
}

#[test]
fn terminal_apc_handler_ports_unknown_overflow_limit_and_valid_kitty() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    for input in [
        b"\x1b_Xabcdef1234\x1b\\".as_slice(),
        b"\x1b_Gabcdef1234\x1b\\",
        b"\x1b_Ga=p,i=10000000000\x1b\\",
        b"\x1b_Ga=p,i=1,z=-9999999999\x1b\\",
    ] {
        engine.write_vt(input);
        assert!(engine.extract_frame()?.images.placements.is_empty());
    }
    assert_pty_output_empty(&output);

    engine.terminal.set_apc_max_bytes_kitty(Some(2))?;
    engine.write_vt(b"\x1b_Ga=T,f=24,s=1,v=1,i=80;AAAA\x1b\\");
    assert!(engine.extract_frame()?.images.placements.is_empty());

    engine.terminal.set_apc_max_bytes_kitty(None)?;
    engine.write_vt(b"\x1b_Ga=T,f=24,s=1,v=1,i=81;AAAA\x1b\\");
    let frame = engine.extract_frame()?;
    assert!(
        frame
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 81)
    );
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
fn terminal_png_decoder_normalizes_16_bit_frames_to_rgba8() {
    assert_eq!(
        png_frame_to_rgba8(
            &[0x12, 0x34, 0xAB, 0xCD, 0xFF, 0x00],
            png::ColorType::Rgb,
            png::BitDepth::Sixteen,
        ),
        Some(vec![0x12, 0xAB, 0xFF, 255])
    );
    assert_eq!(
        png_frame_to_rgba8(
            &[0x01, 0x02, 0x23, 0x45, 0x67, 0x89, 0xFE, 0xDC],
            png::ColorType::Rgba,
            png::BitDepth::Sixteen,
        ),
        Some(vec![0x01, 0x23, 0x67, 0xFE])
    );
    assert_eq!(
        png_frame_to_rgba8(
            &[0x7F, 0x00, 0x80, 0x00],
            png::ColorType::GrayscaleAlpha,
            png::BitDepth::Sixteen,
        ),
        Some(vec![0x7F, 0x7F, 0x7F, 0x80])
    );
    assert_eq!(
        png_frame_to_rgba8(
            &[0x40, 0x00],
            png::ColorType::Grayscale,
            png::BitDepth::Sixteen
        ),
        Some(vec![0x40, 0x40, 0x40, 255])
    );
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
fn terminal_engine_ports_kitty_image_direct_validation() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b_Ga=t,f=24,t=d,i=61,s=1,v=1;AAAA\x1b\\");
    let image = engine
        .terminal
        .kitty_graphics()?
        .image(61)
        .expect("direct RGB image");
    assert_eq!(
        image.format()?,
        libghostty_vt::kitty::graphics::ImageFormat::Rgb
    );
    assert_eq!(image.data()?.len(), 3);
    assert_kitty_response(&output, 61, "OK");

    for (command, image_id, status) in [
        (
            "\x1b_Ga=t,f=24,t=d,i=62,s=10001,v=1;AAAA\x1b\\",
            62_u32,
            "EINVAL: dimensions too large",
        ),
        (
            "\x1b_Ga=t,f=24,t=d,i=63,s=1,v=10001;AAAA\x1b\\",
            63_u32,
            "EINVAL: dimensions too large",
        ),
    ] {
        let (mut terminal, output) = captured_pty_engine()?;
        terminal.write_vt(command.as_bytes());
        assert_kitty_response(&output, image_id, status);
    }
    Ok(())
}

#[test]
#[ignore = "requires Ghostty kitty zlib fixture that is not vendored in this rewrite"]
fn terminal_engine_ports_kitty_image_zlib_direct_and_chunked() -> Result<()> {
    let payload = base64_encode_bytes(GHOSTTY_RGB_ZLIB_128X96);

    let mut direct = test_terminal_engine()?;
    direct.write_vt(format!("\x1b_Ga=t,f=24,t=d,o=z,i=64,s=128,v=96;{payload}\x1b\\").as_bytes());
    let image = direct
        .terminal
        .kitty_graphics()?
        .image(64)
        .expect("zlib RGB image");
    assert_eq!(
        image.format()?,
        libghostty_vt::kitty::graphics::ImageFormat::Rgb
    );
    assert_eq!(image.data()?.len(), 128 * 96 * 3);

    let split = GHOSTTY_RGB_ZLIB_128X96.len() / 2;
    let mut chunked = test_terminal_engine()?;
    chunked.write_vt(
        format!(
            "\x1b_Ga=t,f=24,t=d,o=z,i=65,s=128,v=96,m=1;{}\x1b\\",
            base64_encode_bytes(&GHOSTTY_RGB_ZLIB_128X96[..split])
        )
        .as_bytes(),
    );
    chunked.write_vt(
        format!(
            "\x1b_Gm=0;{}\x1b\\",
            base64_encode_bytes(&GHOSTTY_RGB_ZLIB_128X96[split..])
        )
        .as_bytes(),
    );
    assert_eq!(
        chunked
            .terminal
            .kitty_graphics()?
            .image(65)
            .expect("chunked zlib image")
            .data()?
            .len(),
        128 * 96 * 3
    );

    let mut zero_initial = test_terminal_engine()?;
    zero_initial.write_vt(b"\x1b_Ga=t,f=24,t=d,o=z,i=66,s=128,v=96,m=1\x1b\\");
    zero_initial.write_vt(format!("\x1b_Gm=0;{payload}\x1b\\").as_bytes());
    assert_eq!(
        zero_initial
            .terminal
            .kitty_graphics()?
            .image(66)
            .expect("zero-initial chunked zlib image")
            .data()?
            .len(),
        128 * 96 * 3
    );
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_image_file_and_tempfile_media() -> Result<()> {
    let rgb_path = write_temp_fixture("image.data", GHOSTTY_RGB_20X15)?;
    let mut regular = test_terminal_engine()?;
    regular.write_vt(
        format!(
            "\x1b_Ga=t,f=24,t=f,i=67,s=20,v=15;{}\x1b\\",
            file_payload(&rgb_path)?
        )
        .as_bytes(),
    );
    assert_eq!(
        regular
            .terminal
            .kitty_graphics()?
            .image(67)
            .expect("regular-file RGB image")
            .data()?
            .len(),
        20 * 15 * 3
    );
    assert!(rgb_path.exists());

    let temp_path = write_temp_fixture("tty-graphics-protocol-image.data", GHOSTTY_RGB_20X15)?;
    let mut temporary = test_terminal_engine()?;
    temporary.write_vt(
        format!(
            "\x1b_Ga=t,f=24,t=t,i=68,s=20,v=15;{}\x1b\\",
            file_payload(&temp_path)?
        )
        .as_bytes(),
    );
    assert_eq!(
        temporary
            .terminal
            .kitty_graphics()?
            .image(68)
            .expect("temporary-file RGB image")
            .data()?
            .len(),
        20 * 15 * 3
    );
    assert!(!temp_path.exists());

    let bad_temp_path = write_temp_fixture("image.data", GHOSTTY_RGB_20X15)?;
    let (mut bad_temp, bad_temp_output) = captured_pty_engine()?;
    bad_temp.write_vt(
        format!(
            "\x1b_Ga=t,f=24,t=t,i=69,s=20,v=15;{}\x1b\\",
            file_payload(&bad_temp_path)?
        )
        .as_bytes(),
    );
    assert_kitty_response(
        &bad_temp_output,
        69,
        "EINVAL: temporary file not named correctly",
    );
    assert!(bad_temp_path.exists());
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
    assert_kitty_response(&shared_memory_output, 71, "EINVAL: unsupported medium");
    Ok(())
}

#[test]
fn terminal_engine_ports_kitty_command_unknown_key_compatibility() {
    let (mut unknown_key, _) = captured_pty_engine().expect("captured pty engine");
    let rgb_payload = base64_encode_bytes(&vec![0xaa; 10 * 20 * 3]);
    unknown_key
        .write_vt(format!("\x1b_Gf=24,s=10,v=20,hello=world,i=74;{rgb_payload}\x1b\\").as_bytes());
    let image = unknown_key
        .terminal
        .kitty_graphics()
        .expect("kitty graphics")
        .image(74)
        .expect("unknown keys ignored");
    assert_eq!(image.width().expect("image width"), 10);
    assert_eq!(image.height().expect("image height"), 20);
    assert_eq!(image.data().expect("image data").len(), 10 * 20 * 3);
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
fn terminal_engine_ports_kitty_cleanup_commands_and_storage_disable() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(ONE_PIXEL_PNG_APC.as_bytes());
    engine.write_vt(b"\x1b_Ga=p,i=31,p=2,q=1\x1b\\");
    let frame = engine.extract_frame()?;
    let mut placement_ids = frame
        .images
        .placements
        .iter()
        .map(|placement| placement.placement_id)
        .collect::<Vec<_>>();
    placement_ids.sort_unstable();
    assert_eq!(placement_ids, vec![1, 2]);

    engine.write_vt(b"\x1b_Ga=d,d=i,i=31,p=2\x1b\\");
    let frame = engine.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].placement_id, 1);
    assert!(engine.terminal.kitty_graphics()?.image(31).is_some());

    engine.write_vt(b"\x1b_Ga=d,d=a\x1b\\");
    let frame = engine.extract_frame()?;
    assert!(frame.images.placements.is_empty());
    assert!(engine.terminal.kitty_graphics()?.image(31).is_some());

    engine.write_vt(b"\x1b_Ga=p,i=31,p=3,q=1\x1b\\");
    let frame = engine.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].placement_id, 3);

    engine.set_kitty_image_storage_limit(0)?;
    let frame = engine.extract_frame()?;
    assert!(frame.images.placements.is_empty());
    assert!(engine.terminal.kitty_graphics()?.image(31).is_none());

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
fn terminal_engine_ports_kitty_storage_delete_by_id_placement_and_range() -> Result<()> {
    let mut engine = storage_test_engine()?;

    for (image_id, placement_id) in [(1, 1), (1, 2), (2, 1), (3, 1)] {
        engine.write_vt(raw_rgba_command(image_id, placement_id, 1, 1).as_bytes());
    }
    assert_eq!(
        image_placement_ids(engine.extract_frame()?),
        [(1, 1), (1, 2), (2, 1), (3, 1)]
    );

    engine.write_vt(b"\x1b_Ga=d,d=i,i=2\x1b\\");
    assert_eq!(
        image_placement_ids(engine.extract_frame()?),
        [(1, 1), (1, 2), (3, 1)]
    );
    assert!(engine.terminal.kitty_graphics()?.image(2).is_some());

    engine.write_vt(b"\x1b_Ga=d,d=I,i=3\x1b\\");
    assert_eq!(
        image_placement_ids(engine.extract_frame()?),
        [(1, 1), (1, 2)]
    );
    assert!(engine.terminal.kitty_graphics()?.image(3).is_none());

    engine.write_vt(b"\x1b_Ga=d,d=I,i=1,p=2\x1b\\");
    assert_eq!(image_placement_ids(engine.extract_frame()?), [(1, 1)]);

    engine.write_vt(raw_rgba_command(2, 1, 1, 1).as_bytes());
    engine.write_vt(raw_rgba_command(3, 1, 1, 1).as_bytes());
    engine.write_vt(b"\x1b_Ga=d,d=r,x=1,y=2\x1b\\");
    assert!(image_placement_ids(engine.extract_frame()?).is_empty());
    assert!(engine.terminal.kitty_graphics()?.image(1).is_some());
    assert!(engine.terminal.kitty_graphics()?.image(2).is_some());

    engine.write_vt(raw_rgba_command(1, 1, 1, 1).as_bytes());
    engine.write_vt(raw_rgba_command(2, 1, 1, 1).as_bytes());
    engine.write_vt(b"\x1b_Ga=d,d=R,x=1,y=2\x1b\\");
    assert!(image_placement_ids(engine.extract_frame()?).is_empty());
    assert!(engine.terminal.kitty_graphics()?.image(1).is_none());
    assert!(engine.terminal.kitty_graphics()?.image(2).is_none());
    assert!(engine.terminal.kitty_graphics()?.image(3).is_some());

    engine.write_vt(b"\x1b_Ga=d,d=A\x1b\\");
    assert!(engine.extract_frame()?.images.placements.is_empty());
    assert!(engine.terminal.kitty_graphics()?.image(3).is_none());
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
fn terminal_engine_inherits_unimplemented_kitty_animation_as_no_state_update() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(ONE_PIXEL_PNG_APC.as_bytes());
    let frame = engine.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);

    for command in [
        b"\x1b_Ga=f,i=31,c=1,q=1\x1b\\".as_slice(),
        b"\x1b_Ga=a,i=31,s=3,q=1\x1b\\".as_slice(),
        b"\x1b_Ga=c,i=31,c=1,q=1\x1b\\".as_slice(),
        b"\x1b_Ga=d,d=f,q=1\x1b\\".as_slice(),
    ] {
        engine.write_vt(command);
    }

    let frame = engine.extract_frame()?;
    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(frame.images.placements[0].image_id, 31);
    assert_eq!(frame.images.placements[0].placement_id, 1);
    assert!(engine.terminal.kitty_graphics()?.image(31).is_some());

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
fn terminal_engine_ports_kitty_default_format_as_rgba() -> Result<()> {
    let (mut engine, output) = captured_pty_engine()?;

    engine.write_vt(b"\x1b_Ga=T,t=d,i=1,s=1,v=2,c=10,r=1;///////////\x1b\\");
    let graphics = engine.terminal.kitty_graphics()?;
    let image = graphics.image(1).expect("image id 1");

    assert_eq!(
        image.format()?,
        libghostty_vt::kitty::graphics::ImageFormat::Rgba
    );
    assert_eq!(image.data()?.len(), 8);
    assert_kitty_response(&output, 1, "OK");
    Ok(())
}

#[test]
fn kitty_graphics_adapter_ports_iterator_image_and_layer_queries() -> Result<()> {
    let empty = test_terminal_engine()?;
    let empty_graphics = empty.terminal.kitty_graphics()?;
    let mut empty_iter = PlacementIterator::new()?;
    assert!(empty_iter.update(&empty_graphics)?.next().is_none());
    assert!(empty_graphics.image(999).is_none());

    let mut engine = test_terminal_engine()?;
    let payload = base64_encode_bytes(&[0xaa; 4 * 3 * 3]);
    engine.write_vt(format!("\x1b_Ga=t,t=d,f=24,i=81,s=4,v=3;{payload}\x1b\\").as_bytes());
    engine.write_vt(b"\x1b_Ga=p,i=81,p=1,c=4,r=2,z=5;\x1b\\");
    engine.write_vt(b"\x1b_Ga=p,i=81,p=2,c=3,r=2,z=-1;\x1b\\");
    engine.write_vt(b"\x1b_Ga=p,i=81,p=3,c=2,r=1,z=-1073741825;\x1b\\");

    let graphics = engine.terminal.kitty_graphics()?;
    let image = graphics.image(81).expect("stored image");
    assert_eq!(image.id()?, 81);
    assert_eq!(image.number()?, 0);
    assert_eq!(image.width()?, 4);
    assert_eq!(image.height()?, 3);
    assert_eq!(
        image.format()?,
        libghostty_vt::kitty::graphics::ImageFormat::Rgb
    );
    assert_eq!(
        image.compression()?,
        libghostty_vt::kitty::graphics::Compression::None
    );
    assert_eq!(image.data()?.len(), 4 * 3 * 3);

    let mut iter = PlacementIterator::new()?;
    let mut placements = iter.update(&graphics)?;
    let mut all = Vec::new();
    while let Some(placement) = placements.next() {
        all.push((
            placement.image_id()?,
            placement.placement_id()?,
            placement.is_virtual()?,
            placement.columns()?,
            placement.rows()?,
            placement.z()?,
        ));
    }
    all.sort_unstable_by_key(|(_, placement_id, _, _, _, _)| *placement_id);
    assert_eq!(
        all,
        vec![
            (81, 1, false, 4, 2, 5),
            (81, 2, false, 3, 2, -1),
            (81, 3, false, 2, 1, -1_073_741_825),
        ]
    );

    let mut above = PlacementIterator::new()?;
    let mut placements = above.update(&graphics)?;
    placements.set_layer(Layer::AboveText)?;
    assert_eq!(
        placements
            .next()
            .expect("above-text placement")
            .placement_id()?,
        1
    );
    assert!(placements.next().is_none());

    let mut below_text = PlacementIterator::new()?;
    let mut placements = below_text.update(&graphics)?;
    placements.set_layer(Layer::BelowText)?;
    assert_eq!(
        placements
            .next()
            .expect("below-text placement")
            .placement_id()?,
        2
    );
    assert!(placements.next().is_none());

    let mut below_bg = PlacementIterator::new()?;
    let mut placements = below_bg.update(&graphics)?;
    placements.set_layer(Layer::BelowBg)?;
    assert_eq!(
        placements
            .next()
            .expect("below-background placement")
            .placement_id()?,
        3
    );
    assert!(placements.next().is_none());

    Ok(())
}

#[test]
fn kitty_graphics_adapter_ports_geometry_and_render_info_queries() -> Result<()> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;

    engine.write_vt(raw_rgb_command_with_options(82, 1, "c=5,r=2,x=1,y=1,w=10,h=10").as_bytes());
    {
        let graphics = engine.terminal.kitty_graphics()?;
        let image = graphics.image(82).expect("stored image");
        let mut iter = PlacementIterator::new()?;
        let mut placements = iter.update(&graphics)?;
        let placement = placements.next().expect("placement");

        assert_eq!(placement.pixel_size(&image, &engine.terminal)?.width, 50);
        assert_eq!(placement.pixel_size(&image, &engine.terminal)?.height, 40);
        assert_eq!(placement.grid_size(&image, &engine.terminal)?.cols, 5);
        assert_eq!(placement.grid_size(&image, &engine.terminal)?.rows, 2);
        assert_eq!(
            placement.viewport_pos(&image, &engine.terminal)?,
            Some(libghostty_vt::kitty::graphics::ViewportPos { col: 0, row: 0 })
        );
        assert_eq!(
            placement.source_rect(&image)?,
            libghostty_vt::kitty::graphics::SourceRect {
                x: 1,
                y: 1,
                width: 3,
                height: 2,
            }
        );
        let rect = placement.rect(&image, &engine.terminal)?;
        assert!(rect.rectangle);

        let info = placement.placement_render_info(&image, &engine.terminal)?;
        assert_eq!(info.pixel_width, 50);
        assert_eq!(info.pixel_height, 40);
        assert_eq!(info.grid_cols, 5);
        assert_eq!(info.grid_rows, 2);
        assert_eq!(info.viewport_col, 0);
        assert_eq!(info.viewport_row, 0);
        assert!(info.viewport_visible);
        assert_eq!(info.source_x, 1);
        assert_eq!(info.source_y, 1);
        assert_eq!(info.source_width, 3);
        assert_eq!(info.source_height, 2);
    }

    let mut offscreen = TerminalEngine::new(TerminalGeometry {
        cols: 10,
        rows: 4,
        cell_width: 10,
        cell_height: 20,
    })?;
    offscreen.write_vt(b"\x1b_Ga=T,t=d,f=24,i=83,s=1,v=1,q=1;////\x1b\\");
    offscreen.write_vt(b"\x1b_Ga=p,U=1,i=83,p=1,c=1,r=1,q=1\x1b\\");
    offscreen.write_vt("\u{10EEEE}".as_bytes());
    let graphics = offscreen.terminal.kitty_graphics()?;
    let image = graphics.image(83).expect("offscreen image");
    let mut iter = PlacementIterator::new()?;
    let mut placements = iter.update(&graphics)?;
    let placement = placements.next().expect("virtual placement");
    assert!(placement.is_virtual()?);
    assert_eq!(placement.viewport_pos(&image, &offscreen.terminal)?, None);
    assert!(
        !placement
            .placement_render_info(&image, &offscreen.terminal)?
            .viewport_visible
    );

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
    engine.write_vt("\u{10EEEE}".as_bytes());
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
    assert_eq!(placement.destination.height(), 8.0);

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
fn terminal_engine_reuses_kitty_image_payload_across_frame_extraction() -> Result<()> {
    let mut engine = test_terminal_engine()?;

    engine.write_vt(ONE_PIXEL_PNG_APC.as_bytes());
    let first = engine.extract_frame()?.images.placements[0].data.clone();
    let second = engine.extract_frame()?.images.placements[0].data.clone();

    assert!(
        Arc::ptr_eq(&first, &second),
        "unchanged Kitty image payload should not be copied every frame"
    );
    Ok(())
}
