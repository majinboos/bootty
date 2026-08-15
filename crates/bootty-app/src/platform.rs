use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::{BoottyConfig, MacosTitlebarStyle, WindowConfig};

pub fn read_clipboard_text() -> Result<Option<String>> {
    if let Some(paths) = bootty_winit::file_paths::read_clipboard_file_paths()
        && let Some(text) = bootty_winit::file_paths::format_file_paths_for_paste(
            paths.iter().map(PathBuf::as_path),
        )
    {
        return Ok(Some(text));
    }

    let mut clipboard = arboard::Clipboard::new()?;
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => Ok(Some(text)),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => read_clipboard_image_as_path(),
        Err(error) => Err(error.into()),
    }
}

/// A clipboard holding image bytes and no text (a screenshot, a copied image) has nothing to
/// paste as text, so paste the image itself: spill it to a PNG under the temp dir and paste
/// that path. Same shape as the copied-file case above, which pastes paths rather than bytes,
/// and it gives programs that read paths — editors, agents, `open` — something to work with.
fn read_clipboard_image_as_path() -> Result<Option<String>> {
    let mut clipboard = arboard::Clipboard::new()?;
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let path = write_clipboard_image_png(&image)?;
    Ok(bootty_winit::file_paths::format_file_paths_for_paste([
        path.as_path(),
    ]))
}

fn write_clipboard_image_png(image: &arboard::ImageData<'_>) -> Result<PathBuf> {
    let width = u32::try_from(image.width).context("clipboard image width")?;
    let height = u32::try_from(image.height).context("clipboard image height")?;
    let path = std::env::temp_dir().join(format!("bootty-clipboard-{}.png", clipboard_image_id()));

    let file = std::fs::File::create(&path)
        .with_context(|| format!("create clipboard image file {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .context("write clipboard image header")?
        .write_image_data(&image.bytes)
        .context("write clipboard image data")?;
    Ok(path)
}

/// Unique per paste without pulling in a uuid dependency: the process id pins the writer and the
/// counter pins the paste, so two pastes never land on one file.
fn clipboard_image_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn write_clipboard_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_owned())?;
    Ok(())
}

pub fn write_clipboard_html(html: &str, plain_text: Option<&str>) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_html(html.to_owned(), plain_text.map(str::to_owned))?;
    Ok(())
}

pub fn show_desktop_notification(title: &str, body: &str) -> Result<()> {
    platform_show_desktop_notification(title, body)
}

#[cfg(target_os = "macos")]
fn platform_show_desktop_notification(title: &str, body: &str) -> Result<()> {
    let script = format!(
        "display notification {} with title {}",
        osascript_quote(body),
        osascript_quote(title)
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_show_desktop_notification(title: &str, body: &str) -> Result<()> {
    std::process::Command::new("notify-send")
        .args([title, body])
        .spawn()?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_show_desktop_notification(_title: &str, _body: &str) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn osascript_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub fn apply_macos_non_native_fullscreen_presentation(window: &WindowConfig) -> bool {
    bootty_winit::window::set_macos_non_native_fullscreen(
        window.non_native_fullscreen_enabled()
            && window.hides_macos_menu_bar_in_non_native_fullscreen(),
    )
}

pub fn macos_handles_non_native_fullscreen_frame(window: &WindowConfig) -> bool {
    window.non_native_fullscreen_enabled()
        && window.hides_macos_menu_bar_in_non_native_fullscreen()
        && bootty_winit::window::handles_macos_non_native_fullscreen_frame()
}

pub fn native_options_for_config(config: &BoottyConfig) -> eframe::NativeOptions {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(config.window.title.clone())
        .with_inner_size([config.window.width, config.window.height])
        .with_decorations(config.window.decorations_enabled())
        .with_fullscreen(config.window.native_fullscreen_enabled())
        .with_maximized(
            config.window.non_native_fullscreen_enabled()
                && !macos_handles_non_native_fullscreen_frame(&config.window),
        );
    viewport = apply_native_icon_to_viewport(viewport);

    viewport = match config.window.macos_titlebar_style {
        MacosTitlebarStyle::Native => viewport,
        MacosTitlebarStyle::Transparent => viewport
            .with_title_shown(false)
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true),
        MacosTitlebarStyle::Hidden => viewport
            .with_title_shown(false)
            .with_titlebar_buttons_shown(false)
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true),
    };

    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        ..Default::default()
    }
}

#[cfg(target_os = "macos")]
pub fn new_tab_shortcut_trigger() -> &'static str {
    "cmd+t"
}

#[cfg(not(target_os = "macos"))]
pub fn new_tab_shortcut_trigger() -> &'static str {
    "ctrl+shift+t"
}

#[cfg(target_os = "macos")]
fn apply_native_icon_to_viewport(
    viewport: eframe::egui::ViewportBuilder,
) -> eframe::egui::ViewportBuilder {
    viewport.with_icon(eframe::egui::IconData::default())
}

#[cfg(not(target_os = "macos"))]
fn apply_native_icon_to_viewport(
    viewport: eframe::egui::ViewportBuilder,
) -> eframe::egui::ViewportBuilder {
    viewport.with_icon(crate::assets::native_app_icon_data())
}
