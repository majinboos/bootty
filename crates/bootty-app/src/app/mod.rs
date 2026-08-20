mod state;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{OnceLock, mpsc},
    time::Instant,
};

use anyhow::Result;
use eframe::{
    egui::{
        self, FontData, FontDefinitions, FontFamily, Pos2, Rect, TextureHandle, UiBuilder, Vec2,
    },
    wgpu,
};

pub use state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};
use state::{TerminalProgress, TerminalProgressState};

use crate::{
    config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig},
    direct_input::{DirectKeyInput, ModifierSideState, suppress_egui_events_for_direct_input},
    layout::SplitDirection,
    menu::AppMenu,
    renderer::TerminalWidget,
    terminal_text::TerminalTextConfig,
    theme::{theme_palette_from_config, theme_tokens},
    ui::{
        chrome::{self, SidebarModel, StatusBarModel},
        settings::{SettingsAction, SettingsSurface},
    },
};

/// Fallback layout offset (points) when a notched screen is detected but the exact band can't
/// be measured. This intentionally targets the physical notch, not the slightly lower menu-bar
/// drop-down line reported by macOS safe-area APIs.
const FALLBACK_NOTCH_LAYOUT_OFFSET: f32 = 24.0;
const MACOS_NOTCH_MENU_BAR_OVERSHOOT: f32 = 7.0;
const FULLSCREEN_NOTCH_TAB_ROW_CLEARANCE: f32 = 4.0;
/// Minimum sidebar width enforced while dragging the resize handle (matches the settings floor).
const MIN_SIDEBAR_WIDTH: f32 = 120.0;
/// Grab width of the invisible splitter painted at the sidebar's inner edge.
const SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 8.0;
/// Minimum on-screen size of a pane (px) enforced while dragging a split divider.
const MIN_PANE_PX: f32 = 80.0;
/// Minimum grab width (px) for a split divider handle, so a thin configured divider stays draggable.
const MIN_PANE_DIVIDER_GRAB: f32 = 8.0;
const EGUI_SYMBOL_FALLBACK_FAMILIES: &[&str] = &[
    "Apple Symbols",
    "Segoe UI Symbol",
    "Noto Sans Symbols 2",
    "Noto Sans Symbols",
    "DejaVu Sans",
    "Symbola",
    "Arial Unicode MS",
];
const EGUI_SYMBOL_FALLBACK_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn sidebar_visible_for_spaces(configured: bool, _space_count: usize) -> bool {
    configured
}

fn sidebar_content_height(sidebar_height: f32) -> f32 {
    (sidebar_height - chrome::SPACE_SWITCHER_HEIGHT).max(0.0)
}

fn backend_uses_native_layout_renderer(backend: MultiplexerBackendConfig) -> bool {
    matches!(
        backend,
        MultiplexerBackendConfig::Native | MultiplexerBackendConfig::Rmux
    )
}
fn color_hex(color: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

fn sync_global_egui_style(ctx: &egui::Context, theme: bootty_ui::Theme) {
    let mut style = (*ctx.global_style()).clone();
    bootty_ui::configure_style(&mut style, theme);
    ctx.set_global_style(style);
}

fn status_bar_left_padding(_sidebar_visible: bool, _sidebar_on_right: bool) -> f32 {
    chrome::STATUS_EDGE_PAD
}

fn status_bar_background_color(
    chrome_config: &crate::config::ChromeConfig,
    palette: bootty_ui::ThemePalette,
    notch_chrome_color: Option<egui::Color32>,
) -> egui::Color32 {
    notch_chrome_color
        .or_else(|| {
            chrome_config
                .status_background
                .map(crate::theme::config_color32)
        })
        .unwrap_or(palette.mantle)
}

fn sidebar_background_color(
    palette: bootty_ui::ThemePalette,
    configured: Option<egui::Color32>,
    space_color: [u8; 3],
    tint_sidebar: bool,
) -> egui::Color32 {
    let background = configured.unwrap_or(palette.mantle);
    if !tint_sidebar {
        return background;
    }
    egui::Color32::from_rgb(
        ((u16::from(background.r()) * 7 + u16::from(space_color[0])) / 8) as u8,
        ((u16::from(background.g()) * 7 + u16::from(space_color[1])) / 8) as u8,
        ((u16::from(background.b()) * 7 + u16::from(space_color[2])) / 8) as u8,
    )
}

/// Pane corner radius (px), clamped so it never exceeds the pane's shorter half-extent.
fn pane_corner_radius_px(rect: Rect, px: f32) -> f32 {
    let max = (rect.width().min(rect.height()) / 2.0).max(0.0);
    px.clamp(0.0, max)
}

fn pane_corner_radius(rect: Rect, px: f32) -> egui::CornerRadius {
    egui::CornerRadius::same(pane_corner_radius_px(rect, px).round().clamp(0.0, 255.0) as u8)
}

/// Paint the four corner wedges between each square corner and its rounded arc with `bg`, so a pane
/// reads as rounded (the window background shows through the corners) even though the terminal
/// content itself isn't clipped.
fn paint_pane_corner_masks(painter: &egui::Painter, rect: Rect, radius: f32, bg: egui::Color32) {
    let r = pane_corner_radius_px(rect, radius).round();
    if r <= 0.5 {
        return;
    }
    // (arc center, square-corner point, start angle) for each corner, angles in egui screen space
    // (y down), sweeping a quarter turn. A single mesh avoids anti-aliased cracks between the
    // triangles; separate convex polygons showed one-pixel seams at the pane corners.
    let corners = [
        (
            Pos2::new(rect.min.x + r, rect.min.y + r),
            rect.min,
            std::f32::consts::PI,
        ),
        (
            Pos2::new(rect.max.x - r, rect.min.y + r),
            Pos2::new(rect.max.x, rect.min.y),
            std::f32::consts::FRAC_PI_2 * 3.0,
        ),
        (Pos2::new(rect.max.x - r, rect.max.y - r), rect.max, 0.0),
        (
            Pos2::new(rect.min.x + r, rect.max.y - r),
            Pos2::new(rect.min.x, rect.max.y),
            std::f32::consts::FRAC_PI_2,
        ),
    ];
    let steps = 16;
    let mut mesh = egui::epaint::Mesh::default();
    for (center, corner, start) in corners {
        for step in 0..steps {
            let idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(corner, bg);
            for arc_step in [step, step + 1] {
                let angle = start + std::f32::consts::FRAC_PI_2 * (arc_step as f32 / steps as f32);
                mesh.colored_vertex(
                    Pos2::new(center.x + r * angle.cos(), center.y + r * angle.sin()),
                    bg,
                );
            }
            mesh.add_triangle(idx, idx + 1, idx + 2);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

const TERMINAL_PROGRESS_HEIGHT: f32 = 2.0;
const INDETERMINATE_PROGRESS_WIDTH: f32 = 0.25;
const INDETERMINATE_PROGRESS_CYCLE: f64 = 1.5;

fn terminal_progress_color(
    progress: TerminalProgress,
    palette: bootty_ui::ThemePalette,
) -> egui::Color32 {
    match progress.state {
        TerminalProgressState::Normal | TerminalProgressState::Indeterminate => palette.accent,
        TerminalProgressState::Error => palette.destructive,
        TerminalProgressState::Warning => palette.warning,
    }
}

fn progress_animation_time() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn indeterminate_progress_left(track_width: f32, time: f64) -> f32 {
    let segment_width = track_width * INDETERMINATE_PROGRESS_WIDTH;
    let phase = (time / INDETERMINATE_PROGRESS_CYCLE).fract() as f32;
    let travel = 1.0 - (phase * 2.0 - 1.0).abs();
    (track_width - segment_width).max(0.0) * travel
}

fn animate_indeterminate_progress(window_focused: bool, has_indeterminate_progress: bool) -> bool {
    window_focused && has_indeterminate_progress
}

fn paint_terminal_progress(
    painter: &egui::Painter,
    rect: Rect,
    progress: TerminalProgress,
    palette: bootty_ui::ThemePalette,
    time: f64,
) {
    let track = Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width(), TERMINAL_PROGRESS_HEIGHT.min(rect.height())),
    );
    painter.rect_filled(track, 0.0, palette.border);
    let (fill_left, fill_width) = if progress.state == TerminalProgressState::Indeterminate {
        let fill_width = track.width() * INDETERMINATE_PROGRESS_WIDTH;
        (
            track.min.x + indeterminate_progress_left(track.width(), time),
            fill_width,
        )
    } else {
        let Some(fraction) = progress.fraction() else {
            return;
        };
        (track.min.x, track.width() * fraction.clamp(0.0, 1.0))
    };
    let fill = Rect::from_min_size(
        Pos2::new(fill_left, track.min.y),
        egui::vec2(fill_width, track.height()),
    );
    painter.rect_filled(fill, 0.0, terminal_progress_color(progress, palette));
}

/// Shrink a divider's visual rect along its long axis by the pane corner radius (per end), so it
/// stops where the adjacent panes round off rather than crossing the rounded corners.
fn inset_divider_for_radius(rect: Rect, direction: SplitDirection, radius: f32) -> Rect {
    match direction {
        SplitDirection::Right => {
            let inset = radius.clamp(0.0, rect.height() / 2.0);
            Rect::from_min_max(
                Pos2::new(rect.min.x, rect.min.y + inset),
                Pos2::new(rect.max.x, rect.max.y - inset),
            )
        }
        SplitDirection::Down => {
            let inset = radius.clamp(0.0, rect.width() / 2.0);
            Rect::from_min_max(
                Pos2::new(rect.min.x + inset, rect.min.y),
                Pos2::new(rect.max.x - inset, rect.max.y),
            )
        }
    }
}

fn fullscreen_notch_layout_offset(configured_offset: Option<f32>, measured_band: f32) -> f32 {
    if let Some(offset) = configured_offset {
        return offset.max(0.0);
    }

    if measured_band > 0.0 {
        (measured_band - MACOS_NOTCH_MENU_BAR_OVERSHOOT).max(0.0)
    } else {
        FALLBACK_NOTCH_LAYOUT_OFFSET
    }
}

fn fullscreen_status_top_offset(
    fullscreen_top_offset: f32,
    status_row_height: f32,
    extra_tab_rows_clear_notch: bool,
    auto_top_offset: bool,
) -> f32 {
    if extra_tab_rows_clear_notch && auto_top_offset {
        (fullscreen_top_offset + FULLSCREEN_NOTCH_TAB_ROW_CLEARANCE - status_row_height).max(0.0)
    } else {
        fullscreen_top_offset
    }
}

fn fullscreen_status_content_offset(
    tabs_in_notch: bool,
    status_top_offset: f32,
    terminal_cell_height: f32,
    extra_tab_rows_clear_notch: bool,
    auto_top_offset: bool,
) -> f32 {
    if !tabs_in_notch || (extra_tab_rows_clear_notch && auto_top_offset) {
        status_top_offset
    } else {
        (status_top_offset - terminal_cell_height).max(0.0)
    }
}

/// Which extension worker owns the currently-shown Luau window, so its action
/// routes back to the right host (window ids are only unique within a host).
#[derive(Clone, Copy)]
enum LuaWindowOwner {
    Sidebar,
    Status,
}

pub struct BoottyApp {
    state: AppState,
    /// The focused pane's renderer (and the sole renderer for non-native backends). Keeping the
    /// focused widget here means all the zoom/metrics/cell-size logic that reads `terminal_widget`
    /// keeps targeting the focused pane.
    terminal_widget: TerminalWidget,
    /// Renderers for the non-focused panes of a native split, keyed by pane id. On a focus change
    /// the relevant widget is swapped in/out of `terminal_widget` so each pane keeps its own caches.
    pane_widgets: HashMap<String, TerminalWidget>,
    /// Pane id currently held by `terminal_widget` (native split only); `None` for the single-surface
    /// path.
    focused_widget_key: Option<String>,
    /// Held so freshly-split panes get a renderer with the right WGPU target and text config.
    terminal_target_format: Option<wgpu::TextureFormat>,
    terminal_text_config: TerminalTextConfig,
    app_icon_texture: Option<TextureHandle>,
    settings_open: bool,
    settings: SettingsSurface,
    error_details_open: bool,
    // Held for the process lifetime so the native menu stays installed.
    _menu: Option<AppMenu>,
    _control_server: Option<crate::control::ControlServer>,
    _control_plane: crate::control::ControlPlane,
    status_extensions: crate::extensions::ExtensionHost,
    _command_extensions: crate::command_extensions::CommandExtensionHost,
    sidebar_extensions: crate::extensions::ExtensionHost,
    extension_theme: Vec<(String, String)>,
    lua_window: Option<(LuaWindowOwner, crate::ui::lua_window::LuaWindowDialog)>,
    keep_awake: Option<keepawake::KeepAwake>,
    terminal_cursor_icon: egui::CursorIcon,
    sidebar_space_swipe: chrome::SidebarSpaceSwipeState,
    /// Whether the window had keyboard focus this frame. Extension hosts throttle themselves while
    /// it is false, so an unfocused window stops animating (and repainting) its chrome.
    window_focused: bool,
}

impl BoottyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        Self::new_with_config(cc, BoottyConfig::default())
    }

    pub fn new_with_config(cc: &eframe::CreationContext<'_>, config: BoottyConfig) -> Result<Self> {
        Self::new_inner(cc, config, "main".to_owned(), None, None)
    }

    pub fn new_with_direct_input(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        window_state_key: String,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
    ) -> Result<Self> {
        Self::new_inner(
            cc,
            config,
            window_state_key,
            Some(direct_input_rx),
            Some(modifier_side_rx),
        )
    }

    pub fn new_with_control(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        window_state_key: String,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
    ) -> Result<Self> {
        let mut app = Self::new_inner(
            cc,
            config,
            window_state_key.clone(),
            Some(direct_input_rx),
            Some(modifier_side_rx),
        )?;
        app._control_server = Some(crate::control::ControlServer::spawn(
            window_state_key,
            app.state
                .app_command_sender(crate::commands::Caller::Socket),
            app.state.command_catalog(),
            app._control_plane.clone(),
        )?);
        Ok(app)
    }

    fn new_inner(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        window_state_key: String,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        if uses_custom_egui_fonts(&config) {
            configure_egui_fonts(&cc.egui_ctx, config.font.ui_families());
        } else {
            crate::ui::icons::install_icon_fonts(&cc.egui_ctx);
        }
        let repaint_ctx = cc.egui_ctx.clone();
        let repaint: crate::mux::RepaintHandle =
            std::sync::Arc::new(move || repaint_ctx.request_repaint());
        let text_config = config.font.terminal_text_config();
        let target_format = cc
            .wgpu_render_state
            .as_ref()
            .map(|render_state| render_state.target_format);
        let terminal_widget =
            TerminalWidget::new(target_format).with_text_config(text_config.clone());

        // User extensions live beside the config file. Built-ins are Luau modules;
        // user `.lua` / `.luau` files override same-named defaults per extension surface.
        let startup_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let extension_theme = theme_tokens(&config, startup_variant);
        let config_dir = config
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let status_extensions = crate::extensions::ExtensionHost::spawn_status(
            config_dir.join("status"),
            cc.egui_ctx.clone(),
            extension_theme.clone(),
        );
        let sidebar_extensions = crate::extensions::ExtensionHost::spawn_sidebar(
            config_dir.join("sidebar"),
            cc.egui_ctx.clone(),
            extension_theme.clone(),
        );

        let state = AppState::new_for_window(
            config.clone(),
            window_state_key,
            repaint,
            direct_input_rx,
            modifier_side_rx,
        )?;
        let control_plane = crate::control::ControlPlane::default();
        let command_extensions = crate::command_extensions::CommandExtensionHost::load(
            &config_dir.join("extensions"),
            state.command_catalog(),
            control_plane.clone(),
        );
        Ok(Self {
            state,
            terminal_widget,
            pane_widgets: HashMap::new(),
            focused_widget_key: None,
            terminal_target_format: target_format,
            terminal_text_config: text_config,
            app_icon_texture: None,
            settings_open: false,
            settings: SettingsSurface::new(config.clone()),
            error_details_open: false,
            _menu: crate::menu::install(),
            _control_server: None,
            _command_extensions: command_extensions,
            _control_plane: control_plane,
            status_extensions,
            sidebar_extensions,
            extension_theme,
            lua_window: None,
            keep_awake: None,
            terminal_cursor_icon: egui::CursorIcon::Text,
            sidebar_space_swipe: chrome::SidebarSpaceSwipeState::default(),
            window_focused: true,
        })
    }

    fn sync_extension_theme(&mut self, ctx: &egui::Context) {
        if self.state.theme_picker_preview_active() {
            return;
        }
        let next = theme_tokens(self.state.config(), self.state.active_appearance_variant());
        if self.extension_theme == next {
            return;
        }
        self.dismiss_lua_window();
        self.extension_theme = next.clone();
        let config_dir = self
            .state
            .config()
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        self.status_extensions = crate::extensions::ExtensionHost::spawn_status(
            config_dir.join("status"),
            ctx.clone(),
            next.clone(),
        );
        self.sidebar_extensions = crate::extensions::ExtensionHost::spawn_sidebar(
            config_dir.join("sidebar"),
            ctx.clone(),
            next,
        );
        ctx.request_repaint();
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        self.settings_open = true;
        self.state.set_settings_open(true);
        ctx.request_repaint();
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        if !self.settings_open {
            // Covers closes that bypass the Close return below (e.g. a toggle
            // keybind); idempotent once the style is already restored.
            self.settings.restore_global_style(ui.ctx());
            return;
        }
        let theme = self.state.ui_theme();
        let captured_chords = self.state.take_settings_capture_chords();
        let modifier_sides = self.state.modifier_sides();
        if self
            .settings
            .show(ui, theme, captured_chords, modifier_sides)
            == SettingsAction::Close
        {
            self.settings_open = false;
            self.state.set_settings_open(false);
            self.settings.restore_global_style(ui.ctx());
            ui.ctx().request_repaint();
        }
    }

    fn apply_effects(&mut self, ctx: &egui::Context, effects: Vec<AppEffect>) {
        for effect in effects {
            match effect {
                AppEffect::CloseWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                AppEffect::QuitApplication => {
                    let viewport_ids =
                        ctx.input(|input| input.raw.viewports.keys().copied().collect::<Vec<_>>());
                    for viewport_id in viewport_ids {
                        ctx.send_viewport_cmd_to(viewport_id, egui::ViewportCommand::Close);
                    }
                }
                AppEffect::SetWindowTitle(title) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                }
                AppEffect::SetFullscreen(fullscreen) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
                }
                AppEffect::SetMaximized(maximized) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(maximized));
                }
                AppEffect::SetDecorations(decorations) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(decorations));
                }
                AppEffect::RequestCopy => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
                }
                AppEffect::RequestRepaint => ctx.request_repaint(),
                AppEffect::Bell => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                }
                AppEffect::RepaintAfter(after) => ctx.request_repaint_after(after),
                AppEffect::SetTerminalTextConfig(text_config) => {
                    self.terminal_text_config = text_config.clone();
                    self.terminal_widget.set_text_config(text_config.clone());
                    for widget in self.pane_widgets.values_mut() {
                        widget.set_text_config(text_config.clone());
                    }
                }
                AppEffect::SetTerminalCursorIcon(icon) => {
                    self.terminal_cursor_icon = icon;
                    self.terminal_widget.set_terminal_cursor_icon(icon);
                    for widget in self.pane_widgets.values_mut() {
                        widget.set_terminal_cursor_icon(icon);
                    }
                }
                AppEffect::SetUiFonts(families) => {
                    configure_egui_fonts(ctx, &families);
                }
                AppEffect::SetWindowFocus => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                AppEffect::OpenUrl(url) => {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
                AppEffect::OpenSettings => self.open_settings(ctx),
                AppEffect::ConfigureKeybind(action) => {
                    self.open_settings(ctx);
                    self.settings.focus_keybinding(&action);
                }
            }
        }
    }

    fn resolve_status_segments(
        &self,
        segments: &[crate::config::StatusSegment],
    ) -> Vec<chrome::ResolvedSegment> {
        let mux_view = self.current_extension_mux_view(self.state.config().chrome.sidebar);
        let palette = self.state.ui_theme().palette;
        let windows_theme = crate::extensions::BuiltinWindowsTheme {
            accent: palette.accent,
            surface: palette.surface,
            base: palette.base,
            subtext: palette.subtext,
            text: palette.text,
            border: palette.border,
        };

        segments
            .iter()
            .enumerate()
            .filter_map(|(source_slot, segment)| {
                let seg_fg = segment.fg.map(crate::theme::config_color32);
                let seg_bg = segment.bg.map(crate::theme::config_color32);
                let module_items = if segment.module == "windows"
                    && !self.status_extensions.has_user_module("windows")
                {
                    crate::extensions::builtin_windows_items(&mux_view, windows_theme)
                } else {
                    self.status_extensions.items(&segment.module)
                };
                let items = module_items
                    .into_iter()
                    .map(|item| chrome::ResolvedItem {
                        text: item.text,
                        icon: item.icon.or_else(|| segment.icon.clone()),
                        stroke: item.stroke,
                        fg: item.fg.or(seg_fg),
                        bg: item.bg.or(seg_bg),
                        gauge: item.gauge,
                        primitives: item.primitives,
                        pad_left: item.pad_left,
                        pad_right: item.pad_right,
                        join: item.join,
                        gap: item.gap,
                        action: item.action,
                        reorder_anchor: item.reorder_anchor,
                        module: segment.module.clone(),
                    })
                    .collect::<Vec<_>>();
                (!items.is_empty()).then_some(chrome::ResolvedSegment {
                    align: segment.align,
                    source_slot,
                    items,
                })
            })
            .collect()
    }

    fn current_extension_mux_view(&self, sidebar_visible: bool) -> crate::extensions::MuxView {
        let selected = self.state.mux().selected_window();
        let mut windows = self
            .state
            .mux()
            .selected_session_windows()
            .iter()
            .map(|window| {
                let active =
                    selected == Some(window.id.as_str()) || (selected.is_none() && window.active);
                let progress = (!active)
                    .then(|| self.state.window_progress(window))
                    .flatten();
                crate::extensions::WindowView {
                    id: window.id.clone(),
                    index: window.index,
                    name: window.name.clone(),
                    active,
                    progress,
                    progress_indeterminate: progress.is_some()
                        && self.state.window_has_indeterminate_progress(window),
                }
            })
            .collect::<Vec<_>>();
        windows.sort_by_key(|window| window.index);
        let sessions = self.current_extension_sessions();
        let selected_session = self.state.mux().selected_session();
        let selected = sessions.iter().find(|candidate| {
            if let Some(selected) = selected_session {
                candidate.id == selected || candidate.name == selected
            } else {
                candidate.active
            }
        });
        // `bootty.session()` names the session for the status bar, so it reads the same name the
        // sidebar shows rather than the backend's.
        let session = selected.map(|session| {
            if session.display_name.is_empty() {
                session.name.clone()
            } else {
                session.display_name.clone()
            }
        });
        let session_color = selected
            .and_then(|session| session.color.clone())
            .or_else(|| Some(color_hex(self.state.ui_theme().palette.accent)));
        let scope = self.state.mux_scope();
        crate::extensions::MuxView {
            windows,
            sessions,
            scope_key: format!(
                "{}:{}",
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value()
            ),
            session,
            sidebar_visible,
            session_color,
            keep_awake: self.keep_awake.is_some(),
            focused: self.window_focused,
        }
    }

    fn current_status_tab_context(&self) -> Option<chrome::TabContext> {
        let selected_session = self.state.mux().selected_session()?;
        let session =
            self.state.mux().sessions().iter().find(|session| {
                session.id == selected_session || session.name == selected_session
            })?;
        let mut windows = session.windows.iter().collect::<Vec<_>>();
        windows.sort_by_key(|window| window.index);
        let active_window = self
            .state
            .mux()
            .selected_window()
            .or(session.active_window_id.as_deref());
        Some(chrome::TabContext {
            session_id: session.id.clone(),
            targets: windows
                .into_iter()
                .map(|window| chrome::TabContextTarget {
                    window_id: window.id.clone(),
                    is_active: active_window == Some(window.id.as_str()),
                    can_close_pane: window.anchor.pane_id.is_some(),
                })
                .collect(),
        })
    }

    fn current_extension_sessions(&self) -> Vec<crate::extensions::SessionView> {
        let palette = self.state.ui_theme().palette;
        let fallback_color = color_hex(palette.accent);
        let fallback_dim_color = color_hex(palette.muted);
        let selected_session = self.state.mux().selected_session();
        let sessions = self.state.mux().sessions();
        let display_names = self.state.session_display_names(sessions);
        let session_colors = crate::ui::sidebar::sidebar_session_colors(
            sessions,
            &display_names.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .into_iter()
        .map(|entry| {
            (
                entry.session_id.to_owned(),
                (color_hex(entry.color), color_hex(entry.dim_color)),
            )
        })
        .collect::<HashMap<_, _>>();
        sessions
            .iter()
            .zip(display_names)
            .map(|(session, display_name)| {
                let selected = if selected_session.is_some() {
                    selected_session == Some(session.id.as_str())
                        || selected_session == Some(session.name.as_str())
                } else {
                    session.active
                };
                let (color, dim_color) = session_colors
                    .get(&session.id)
                    .cloned()
                    .unwrap_or_else(|| (fallback_color.clone(), fallback_dim_color.clone()));
                let progress = session
                    .windows
                    .iter()
                    .filter_map(|window| self.state.window_progress(window))
                    .max();
                let progress_indeterminate = progress.is_some()
                    && session
                        .windows
                        .iter()
                        .any(|window| self.state.window_has_indeterminate_progress(window));
                let mut reported_panes = HashSet::new();
                let mut progresses = Vec::new();
                for window in &session.windows {
                    for pane in window.panes.iter().chain(std::iter::once(&window.anchor)) {
                        if let Some(pane_id) = pane.pane_id.as_deref()
                            && reported_panes.insert(pane_id)
                            && let Some(progress) = self.state.pane_progress(pane_id)
                        {
                            progresses.push(crate::extensions::SessionProgressView {
                                process: pane
                                    .process
                                    .clone()
                                    .unwrap_or_else(|| "terminal".to_owned()),
                                value: progress.value.unwrap_or(50),
                                indeterminate: progress.state
                                    == TerminalProgressState::Indeterminate,
                            });
                        }
                    }
                }
                crate::extensions::SessionView {
                    id: session.id.clone(),
                    name: session.name.clone(),
                    display_name,
                    active: session.active,
                    selected,
                    cwd: session.anchor.cwd.clone(),
                    pane_id: session.anchor.pane_id.clone(),
                    pane_pid: session.anchor.pane_pid,
                    process: session.anchor.process.clone(),
                    color: Some(color),
                    dim_color: Some(dim_color),
                    progress,
                    progress_indeterminate,
                    progresses,
                    ports: self.state.session_ports(session),
                }
            })
            .collect()
    }

    /// Pushes Bootty-owned mux/session state to extension workers so Luau modules can render it.
    fn publish_extension_mux_view(
        &self,
        sidebar_visible: bool,
        top_bar_visible: bool,
        bottom_bar_visible: bool,
    ) {
        let chrome = &self.state.config().chrome;
        self.status_extensions.set_active(
            top_bar_visible
                .then_some(chrome.top_segments.as_slice())
                .into_iter()
                .chain(bottom_bar_visible.then_some(chrome.bottom_segments.as_slice()))
                .flatten()
                .map(|segment| segment.module.clone()),
        );

        let sidebar_config = &self.state.config().sidebar;
        let session_modules = effective_session_modules(
            sidebar_config,
            self.sidebar_extensions.has_legacy_sessions_module(),
        );
        self.sidebar_extensions.set_active(
            sidebar_visible
                .then_some(
                    sidebar_config.modules.iter().cloned().chain(
                        session_modules
                            .iter()
                            .map(|name| crate::extensions::session_module_key(name)),
                    ),
                )
                .into_iter()
                .flatten(),
        );

        let view = self.current_extension_mux_view(sidebar_visible);
        self.status_extensions.update_mux(view.clone());
        self.sidebar_extensions.update_mux(view);
    }

    fn toggle_keep_awake(&mut self) {
        if self.keep_awake.take().is_some() {
            self.publish_extension_mux_view(
                self.state.config().chrome.sidebar,
                self.state.config().chrome.top_bar,
                self.state.config().chrome.bottom_bar,
            );
            return;
        }

        match keepawake::Builder::default()
            .display(true)
            .idle(true)
            .reason("Bootty status-bar toggle")
            .app_name("Bootty")
            .app_reverse_domain("dev.bootty")
            .create()
        {
            Ok(guard) => self.keep_awake = Some(guard),
            Err(error) => self.state.record_render_error(error),
        }
        self.publish_extension_mux_view(
            self.state.config().chrome.sidebar,
            self.state.config().chrome.top_bar,
            self.state.config().chrome.bottom_bar,
        );
    }
    fn handle_status_bar_event(
        &mut self,
        ctx: &egui::Context,
        status_event: Option<chrome::StatusBarEvent>,
    ) {
        match status_event {
            Some(chrome::StatusBarEvent::Action(action)) => match action.as_str() {
                "toggle-caffeinate" => self.toggle_keep_awake(),
                other => {
                    if let Some(window_id) = other.strip_prefix("activate-window:")
                        && let Some(session_id) =
                            self.state.mux().selected_session().map(str::to_owned)
                    {
                        self.state.activate_window_from_ui(&session_id, window_id);
                    }
                }
            },
            Some(chrome::StatusBarEvent::ContextAction {
                session_id,
                window_id,
                action,
            }) => {
                let handled = match action {
                    chrome::TabContextAction::Activate => {
                        self.state.activate_window_from_ui(&session_id, &window_id);
                        true
                    }
                    chrome::TabContextAction::NewTab => self
                        .state
                        .new_tab_for_window_from_ui(&session_id, &window_id),
                    chrome::TabContextAction::PreviousTab => self
                        .state
                        .activate_relative_window_from_ui(&session_id, &window_id, -1),
                    chrome::TabContextAction::NextTab => self
                        .state
                        .activate_relative_window_from_ui(&session_id, &window_id, 1),
                    chrome::TabContextAction::LastTab => {
                        self.state.activate_last_window_from_ui(&session_id)
                    }
                    chrome::TabContextAction::Rename => self
                        .state
                        .open_rename_tab_dialog_for(&session_id, &window_id),
                    chrome::TabContextAction::MoveLeft => {
                        self.state.move_window_from_ui(&session_id, &window_id, -1)
                    }
                    chrome::TabContextAction::MoveRight => {
                        self.state.move_window_from_ui(&session_id, &window_id, 1)
                    }
                    chrome::TabContextAction::ClosePane => self
                        .state
                        .close_pane_for_window_from_ui(&session_id, &window_id),
                };
                if handled {
                    ctx.request_repaint();
                }
            }
            Some(chrome::StatusBarEvent::Reorder {
                module,
                source,
                before,
            }) => {
                if module == "windows"
                    && self
                        .state
                        .reorder_window_before_from_ui(&source, before.as_deref())
                {
                    ctx.request_repaint();
                } else {
                    self.status_extensions
                        .request_reorder(&module, source, before);
                }
            }
            None => {}
        }
    }

    /// Make `terminal_widget` the renderer for pane `key`, swapping the previous focused pane's
    /// widget back into `pane_widgets` so each pane keeps its own render caches.
    fn focus_pane_widget(&mut self, key: &str) {
        if self.focused_widget_key.as_deref() == Some(key) {
            return;
        }
        if self.focused_widget_key.is_none() {
            self.focused_widget_key = Some(key.to_owned());
            return;
        }
        let mut incoming = self.pane_widgets.remove(key).unwrap_or_else(|| {
            TerminalWidget::new(self.terminal_target_format)
                .with_text_config(self.terminal_text_config.clone())
        });
        incoming.set_terminal_cursor_icon(self.terminal_cursor_icon);
        let outgoing = std::mem::replace(&mut self.terminal_widget, incoming);
        if let Some(old_key) = self.focused_widget_key.replace(key.to_owned()) {
            self.pane_widgets.insert(old_key, outgoing);
        }
    }

    fn show_single_terminal(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        corner_radius_px: f32,
        background: egui::Color32,
    ) {
        match self.terminal_widget.show_at_rect(
            ui,
            rect,
            "primary-terminal",
            self.state.terminal_mut(),
        ) {
            Ok(surface) => self.state.record_surface(surface),
            Err(error) => self.state.record_render_error(error),
        }
        paint_pane_corner_masks(ui.painter(), rect, corner_radius_px, background);
        if let Some(progress) = self.state.current_terminal_progress() {
            paint_terminal_progress(
                ui.painter(),
                rect,
                progress,
                self.state.ui_theme().palette,
                progress_animation_time(),
            );
        }
    }

    /// Render every pane of the active native window into its split sub-rect: the focused pane via
    /// `terminal_widget`, the rest via their own `pane_widgets`. A primary press inside a pane
    /// focuses it. The focused pane gets a thin accent border.
    fn show_split_panes(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        palette: bootty_ui::ThemePalette,
        background: egui::Color32,
    ) {
        let chrome = &self.state.config().chrome;
        let gap = chrome.pane_divider_width;
        let border_width = chrome.pane_focus_border_width;
        let border_color = chrome
            .pane_focus_border_color
            .map(crate::theme::config_color32)
            .unwrap_or(palette.accent);
        let corner_radius_px = chrome.pane_corner_radius;
        let inactive_dim = chrome.unfocused_terminal_dim.clamp(0.0, 1.0);
        let rects = self.state.pane_rects(area, gap);

        // Handle click-to-focus before snapshotting focus so this frame already renders the clicked
        // pane (focus_pane re-syncs the input runtime), avoiding a one-frame stale-pane flash.
        if ui.input(|input| input.pointer.primary_pressed())
            && let Some(pos) = ui.input(|input| input.pointer.interact_pos())
            && let Some((pane_id, _)) = rects.iter().find(|(_, rect)| rect.contains(pos))
        {
            self.state.focus_pane(pane_id);
        }
        let focused = self.state.focused_pane();
        let focused_widget_key = focused
            .as_deref()
            .map(|pane_id| self.state.pane_widget_key(pane_id));
        if let Some(focused_widget_key) = &focused_widget_key {
            self.focus_pane_widget(focused_widget_key);
        }

        let pane_geometries: Vec<(String, String, crate::geometry::TerminalGeometry)> = rects
            .iter()
            .map(|(pane_id, rect)| {
                let widget_key = self.state.pane_widget_key(pane_id);
                let is_focused = focused.as_deref() == Some(pane_id.as_str());
                let geometry = if is_focused {
                    self.terminal_widget.geometry_for_rect(*rect)
                } else {
                    let widget = self
                        .pane_widgets
                        .entry(widget_key.clone())
                        .or_insert_with(|| {
                            TerminalWidget::new(self.terminal_target_format)
                                .with_text_config(self.terminal_text_config.clone())
                        });
                    widget.set_terminal_cursor_icon(self.terminal_cursor_icon);
                    widget.geometry_for_rect(*rect)
                };
                (pane_id.clone(), widget_key, geometry)
            })
            .collect();
        if let Some((cols, rows)) = self.state.pane_terminal_window_size(|pane| {
            pane_geometries
                .iter()
                .find(|(pane_id, _, _)| pane_id.as_str() == pane)
                .map(|(_, _, geometry)| (geometry.cols, geometry.rows))
        }) && let Err(error) = self.state.resize_native_layout_window(cols, rows)
        {
            self.state.record_render_error(error);
        }
        let current_ids: std::collections::HashSet<String> = pane_geometries
            .iter()
            .map(|(_, key, _)| key.clone())
            .collect();
        let Self {
            state,
            terminal_widget,
            pane_widgets,
            terminal_target_format,
            terminal_text_config,
            terminal_cursor_icon,
            ..
        } = self;
        for (pane_id, rect) in &rects {
            let widget_key = state.pane_widget_key(pane_id);
            let is_focused = focused.as_deref() == Some(pane_id.as_str());
            let result = if is_focused {
                Some(terminal_widget.show_at_rect(
                    ui,
                    *rect,
                    ("native-pane", &widget_key),
                    state.terminal_mut(),
                ))
            } else {
                let widget = pane_widgets.entry(widget_key.clone()).or_insert_with(|| {
                    TerminalWidget::new(*terminal_target_format)
                        .with_text_config(terminal_text_config.clone())
                });
                widget.set_terminal_cursor_icon(*terminal_cursor_icon);
                state.render_source_for_pane(pane_id).map(|source| {
                    widget.show_at_rect(ui, *rect, ("native-pane", &widget_key), source)
                })
            };
            match result {
                Some(Ok(surface)) if is_focused => state.record_surface(surface),
                Some(Ok(_)) | None => {}
                Some(Err(error)) => state.record_render_error(error),
            }
            let corner = pane_corner_radius(*rect, corner_radius_px);
            if !is_focused && inactive_dim > 0.0 {
                ui.painter().rect_filled(
                    *rect,
                    corner,
                    egui::Color32::from_black_alpha((inactive_dim * 255.0) as u8),
                );
            }
            // Round the pane by masking its corners with the window background.
            paint_pane_corner_masks(ui.painter(), *rect, corner_radius_px, background);
            if let Some(progress) = state.pane_progress(pane_id) {
                paint_terminal_progress(
                    ui.painter(),
                    *rect,
                    progress,
                    palette,
                    progress_animation_time(),
                );
            }
            if is_focused && border_width > 0.0 {
                ui.painter().rect_stroke(
                    *rect,
                    corner,
                    egui::Stroke::new(border_width, border_color),
                    egui::StrokeKind::Inside,
                );
            }
        }
        // Drop renderers for panes that have closed so their caches don't linger.
        pane_widgets.retain(|key, _| current_ids.contains(key));
    }

    /// Draw a draggable handle over each split divider (mirroring the sidebar splitter). Dragging
    /// adjusts that split's ratio, clamped so neither child shrinks below `MIN_PANE_PX`; the handle
    /// rect is registered so the drag never starts a terminal text selection.
    fn show_pane_dividers(
        &mut self,
        ui: &mut egui::Ui,
        area: Rect,
        palette: bootty_ui::ThemePalette,
        divider_color_override: Option<egui::Color32>,
    ) {
        let chrome = &self.state.config().chrome;
        let gap = chrome.pane_divider_width;
        let corner_radius = chrome.pane_corner_radius;
        let divider_color = divider_color_override.unwrap_or_else(|| {
            chrome
                .pane_divider_color
                .map(crate::theme::config_color32)
                .unwrap_or(palette.mantle)
        });
        let dividers = self.state.pane_dividers(area, gap);
        for divider in &dividers {
            let direction = divider.direction;
            // Widen the grab area past the (possibly thin) visual divider so it stays draggable.
            let handle_rect = match direction {
                SplitDirection::Right => Rect::from_center_size(
                    divider.rect.center(),
                    egui::vec2(
                        divider.rect.width().max(MIN_PANE_DIVIDER_GRAB),
                        divider.rect.height(),
                    ),
                ),
                SplitDirection::Down => Rect::from_center_size(
                    divider.rect.center(),
                    egui::vec2(
                        divider.rect.width(),
                        divider.rect.height().max(MIN_PANE_DIVIDER_GRAB),
                    ),
                ),
            };
            self.state.register_chrome_handle(handle_rect);
            // Always paint the divider at its configured width so it's visible, not just on hover.
            // Inset its long axis by the pane corner radius so it stops where the rounded panes
            // start, instead of cutting straight across the rounded corners.
            let visual = inset_divider_for_radius(divider.rect, direction, corner_radius);
            if visual.width() >= 1.0 && visual.height() >= 1.0 {
                ui.painter().rect_filled(visual, 0.0, divider_color);
            }
            let response = egui::Area::new(egui::Id::new((
                "bootty-pane-divider",
                divider.path.as_slice(),
            )))
            .order(egui::Order::Foreground)
            .fixed_pos(handle_rect.min)
            .show(ui.ctx(), |ui| {
                let response = ui.allocate_rect(handle_rect, egui::Sense::drag());
                if response.hovered() || response.dragged() {
                    let stroke = egui::Stroke::new(2.0, palette.primary);
                    let painter = ui.painter();
                    match direction {
                        SplitDirection::Right => {
                            let x = handle_rect.center().x;
                            painter.line_segment(
                                [
                                    Pos2::new(x, handle_rect.min.y),
                                    Pos2::new(x, handle_rect.max.y),
                                ],
                                stroke,
                            );
                        }
                        SplitDirection::Down => {
                            let y = handle_rect.center().y;
                            painter.line_segment(
                                [
                                    Pos2::new(handle_rect.min.x, y),
                                    Pos2::new(handle_rect.max.x, y),
                                ],
                                stroke,
                            );
                        }
                    }
                }
                response
            })
            .inner;
            if response.hovered() || response.dragged() {
                ui.set_cursor_icon(match direction {
                    SplitDirection::Right => egui::CursorIcon::ResizeHorizontal,
                    SplitDirection::Down => egui::CursorIcon::ResizeVertical,
                });
            }
            if response.dragged()
                && let Some(pos) = ui.ctx().pointer_interact_pos()
            {
                let extent = match direction {
                    SplitDirection::Right => divider.area.width(),
                    SplitDirection::Down => divider.area.height(),
                } - gap;
                if extent > 1.0 {
                    let min_fraction = (MIN_PANE_PX / extent).clamp(0.05, 0.45);
                    self.state.set_pane_ratio(
                        &divider.path,
                        divider.ratio_at(pos, gap),
                        min_fraction,
                    );
                }
            }
        }
    }

    fn show_fixed_layout(&mut self, ui: &mut egui::Ui) {
        // Chrome handles re-register their rects below; clearing here keeps the set to this frame's
        // handles so the next frame's input pass suppresses selection only over live handles.
        self.state.reset_chrome_handles();
        let rect = ui.max_rect();
        let palette =
            theme_palette_from_config(self.state.config(), self.state.active_appearance_variant());
        let chrome_config = self.state.config().chrome.clone();
        let sidebar_configured = chrome_config.sidebar;
        let sidebar =
            sidebar_visible_for_spaces(sidebar_configured, self.state.space_summaries().len());
        let top_bar = chrome_config.top_bar;
        let bottom_bar = chrome_config.bottom_bar;
        let configured_sidebar_width = chrome_config.sidebar_width;
        let status_height_config = chrome_config.status_height;
        let chrome_gap = chrome_config.gap;
        let fullscreen_chrome = self.state.macos_non_native_fullscreen_active()
            || ui
                .ctx()
                .input(|input| input.viewport().fullscreen.unwrap_or(false));
        // Reserve a top offset in fullscreen to clear the notch. The explicit override applies
        // whenever fullscreen so it works even when auto-detection can't read the notch (a hidden
        // menu bar zeroes safeAreaInsets); the safe-area auto value only fills in when unset.
        if fullscreen_chrome {
            crate::platform::macos_disable_titlebar_separator();
        }
        // Drop the window shadow in fullscreen; its rim otherwise reads as a border around the
        // screen-filling window. Restored when windowed.
        crate::platform::macos_set_window_shadow(!fullscreen_chrome);
        // Detect the notch by display name (stable across fullscreen/menu-bar state) rather than the
        // safe-area inset, which zeroes out when the menu bar is hidden in non-native fullscreen.
        let notch_context = fullscreen_chrome && crate::platform::macos_active_screen_is_notched();
        let black_notch_chrome = notch_context
            && chrome_config.notched_fullscreen_black_chrome
            && self.state.active_appearance_variant() == crate::config::AppearanceVariant::Dark;
        let notch_chrome_color = black_notch_chrome.then_some(egui::Color32::BLACK);
        // Pixel height for the layout offset: the config override, else the measured macOS band
        // calibrated to the physical notch, else a fallback when the band is unreadable.
        let measured_band = crate::platform::macos_active_screen_notch_height();
        let fullscreen_top_offset = if notch_context {
            fullscreen_notch_layout_offset(
                self.state.config().window.fullscreen_top_offset,
                measured_band,
            )
        } else {
            0.0
        };
        // When enabled, the terminal/tab bar sits inside the notch band instead of being pushed
        // entirely below it.
        let tabs_in_notch = notch_context && self.state.config().window.fullscreen_tabs_in_notch;
        let notch_band_color = notch_chrome_color.unwrap_or(palette.base);
        let sidebar_width = if sidebar {
            if sidebar_configured {
                configured_sidebar_width
            } else {
                configured_sidebar_width.max(MIN_SIDEBAR_WIDTH)
            }
        } else {
            0.0
        };
        let gap = if sidebar && sidebar_width > 0.0 && !fullscreen_chrome {
            chrome_gap
        } else {
            0.0
        };
        // Apply session-order changes any extension module requested via `bootty.reorder_session`
        // before publishing the snapshot, so the reordered sessions render on the next tick.
        for reorder in self
            .sidebar_extensions
            .take_session_reorders()
            .into_iter()
            .chain(self.status_extensions.take_session_reorders())
        {
            self.state
                .reorder_session_before(&reorder.source, reorder.before.as_deref());
        }
        self.publish_extension_mux_view(sidebar, top_bar, bottom_bar);
        let (sidebar_module_items, sidebar_footer_items) = if sidebar {
            let mut body = Vec::new();
            let mut footer = Vec::new();
            for name in &self.state.config().sidebar.modules {
                for item in self.sidebar_extensions.items(name) {
                    if item.kind.as_deref() == Some("footer") {
                        footer.push(item);
                    } else {
                        body.push(item);
                    }
                }
            }
            let session_modules = effective_session_modules(
                &self.state.config().sidebar,
                self.sidebar_extensions.has_legacy_sessions_module(),
            );
            body = compose_session_module_items(
                body,
                session_modules.iter().flat_map(|name| {
                    self.sidebar_extensions
                        .items(&crate::extensions::session_module_key(name))
                }),
            );
            (body, footer)
        } else {
            (Vec::new(), Vec::new())
        };
        let spaces = self.state.space_summaries();
        let active_space_appearance = spaces
            .iter()
            .find(|space| space.active)
            .map(|space| (space.color, space.tint_sidebar))
            .unwrap_or((crate::workspace::DEFAULT_SPACE_COLOR, false));
        let space_items = spaces
            .into_iter()
            .map(|space| chrome::SpaceSwitcherItem {
                id: space.id,
                name: space.name,
                icon: space.icon,
                color: space.color,
                active: space.active,
                error: space.error,
            })
            .collect::<Vec<_>>();
        let space_transition = self.state.space_transition(std::time::Instant::now());
        let space_switcher_height = chrome::SPACE_SWITCHER_HEIGHT;
        let binding_groups = self.state.binding_session_groups();
        let sidebar_items = if binding_groups.len() > 1 {
            crate::ui::sidebar::build_binding_sidebar_items(&binding_groups)
        } else {
            crate::ui::sidebar::build_sidebar_items_from_module_items(
                &sidebar_module_items,
                self.state.mux_scope(),
                self.state.mux().selected_session(),
                self.state.mux().previous_selected_session().is_some(),
            )
        };
        let sidebar_on_right = matches!(
            self.state.config().sidebar.position,
            crate::config::SidebarPosition::Right
        );
        let clamped_sidebar_width = sidebar_width.min(rect.width());
        let (sidebar_rect, right_rect) = if !sidebar {
            (
                Rect::from_min_size(rect.min, egui::vec2(0.0, rect.height())),
                rect,
            )
        } else if sidebar_on_right {
            let split = (rect.max.x - clamped_sidebar_width).max(rect.min.x);
            (
                Rect::from_min_max(Pos2::new(split, rect.min.y), rect.max),
                Rect::from_min_max(
                    rect.min,
                    Pos2::new((split - gap).max(rect.min.x), rect.max.y),
                ),
            )
        } else {
            let split = (rect.min.x + clamped_sidebar_width).min(rect.max.x);
            (
                Rect::from_min_max(rect.min, Pos2::new(split, rect.max.y)),
                Rect::from_min_max(
                    Pos2::new((split + gap).min(rect.max.x), rect.min.y),
                    rect.max,
                ),
            )
        };
        // When the sidebar is not on the left edge, macOS traffic-light buttons land over the
        // content's top-left instead of the sidebar, so inset the top bar to clear them.
        let top_bar_left_inset = if (!sidebar || sidebar_on_right)
            && self
                .state
                .config()
                .window
                .reserves_macos_titlebar_button_area()
        {
            chrome::MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        } else {
            0.0
        };
        let status_left_padding = status_bar_left_padding(sidebar, sidebar_on_right);
        let top_segments = if top_bar {
            self.resolve_status_segments(&chrome_config.top_segments)
        } else {
            Vec::new()
        };
        let bottom_segments = if bottom_bar {
            self.resolve_status_segments(&chrome_config.bottom_segments)
        } else {
            Vec::new()
        };
        let tab_context = self.current_status_tab_context();
        let top_base_status_height = if top_bar { status_height_config } else { 0.0 };
        let bottom_base_status_height = if bottom_bar {
            status_height_config
        } else {
            0.0
        };
        let notch_span = if tabs_in_notch {
            crate::platform::macos_active_screen_notch_span()
                .map(|(left, right)| (rect.min.x + left, rect.min.x + right))
        } else {
            None
        };
        let candidate_top_status_rect = Rect::from_min_max(
            Pos2::new(
                (right_rect.min.x + top_bar_left_inset).min(right_rect.max.x),
                right_rect.min.y,
            ),
            Pos2::new(right_rect.max.x, right_rect.min.y + top_base_status_height),
        );
        let top_tab_row_count = if top_bar {
            chrome::status_bar_window_tab_row_count(
                ui,
                candidate_top_status_rect,
                &top_segments,
                status_left_padding,
                notch_span,
            )
        } else {
            1
        };
        let candidate_bottom_status_rect = Rect::from_min_max(
            right_rect.min,
            Pos2::new(
                right_rect.max.x,
                right_rect.min.y + bottom_base_status_height,
            ),
        );
        let bottom_tab_row_count = if bottom_bar {
            chrome::status_bar_window_tab_row_count(
                ui,
                candidate_bottom_status_rect,
                &bottom_segments,
                status_left_padding,
                None,
            )
        } else {
            1
        };
        let extra_tab_rows_clear_notch = tabs_in_notch && top_tab_row_count > 1;
        let auto_fullscreen_top_offset = self.state.config().window.fullscreen_top_offset.is_none();
        let status_top_offset = fullscreen_status_top_offset(
            fullscreen_top_offset,
            status_height_config,
            extra_tab_rows_clear_notch,
            auto_fullscreen_top_offset,
        );
        let top_status_height = top_base_status_height * top_tab_row_count as f32;
        let bottom_status_height = bottom_base_status_height * bottom_tab_row_count as f32;
        // Paint the notch band with the sidebar's fullscreen background so the strip above the
        // content matches the sidebar (the sidebar fills its own band). Content draws on top.
        if notch_context && status_top_offset > 0.0 {
            let band = Rect::from_min_max(
                Pos2::new(right_rect.min.x, rect.min.y),
                Pos2::new(right_rect.max.x, rect.min.y + status_top_offset),
            );
            ui.painter().rect_filled(band, 0.0, notch_band_color);
        }
        // With tabs-in-notch the content rises into the notch band and the terminal drops by one
        // row less than the notch so the top bar's bottom edge lines up with the bottom of the
        // notch. The terminal default background is overridden to the band color below so a tmux
        // `bg=default` status line matches the chrome.
        let terminal_cell_height = self.terminal_widget.cell_dimensions().1;
        let content_offset = fullscreen_status_content_offset(
            tabs_in_notch,
            status_top_offset,
            terminal_cell_height,
            extra_tab_rows_clear_notch,
            auto_fullscreen_top_offset,
        );
        let content_top = (right_rect.min.y + content_offset).min(right_rect.max.y);
        let top_status_rect = Rect::from_min_max(
            Pos2::new(
                (right_rect.min.x + top_bar_left_inset).min(right_rect.max.x),
                content_top,
            ),
            Pos2::new(
                right_rect.max.x,
                (content_top + top_status_height).min(right_rect.max.y),
            ),
        );
        let bottom_status_top =
            (right_rect.max.y - bottom_status_height).max(top_status_rect.max.y);
        let bottom_status_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, bottom_status_top),
            right_rect.max,
        );
        let terminal_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, top_status_rect.max.y),
            Pos2::new(right_rect.max.x, bottom_status_rect.min.y),
        );

        if sidebar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    let title_visible = self.state.config().window.custom_chrome_title_visible();
                    // Traffic lights stay at the window's top-left, so only reserve their space in
                    // the sidebar when it is on the left edge.
                    let reserve_titlebar_buttons = !sidebar_on_right
                        && self
                            .state
                            .config()
                            .window
                            .reserves_macos_titlebar_button_area();
                    // Sidebar remains tied to the measured/auto notch offset; extra status tab rows
                    // only change the content/status stack.
                    let top_inset = fullscreen_top_offset;
                    // Resolve `[sidebar]` color overrides on top of the theme. In dark notched
                    // fullscreen the shared notch chrome color overrides all panel backgrounds.
                    let sidebar_cfg = self.state.config().sidebar.clone();
                    let sidebar_background = notch_chrome_color
                        .or_else(|| sidebar_cfg.background.map(crate::theme::config_color32));
                    let mut sidebar_palette = palette;
                    sidebar_palette.base = sidebar_background_color(
                        palette,
                        sidebar_background,
                        active_space_appearance.0,
                        active_space_appearance.1,
                    );
                    if let Some(color) = sidebar_cfg.foreground {
                        sidebar_palette.text = crate::theme::config_color32(color);
                    }
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.painter()
                        .rect_filled(sidebar_rect, 0.0, sidebar_palette.base);
                    if let Some(space_id) = chrome::take_sidebar_space_swipe(
                        ui,
                        sidebar_rect,
                        &space_items,
                        &mut self.sidebar_space_swipe,
                    ) && self.state.activate_space_from_ui(space_id)
                    {
                        ui.ctx().request_repaint();
                    }
                    let title_icon = title_visible.then(|| {
                        chrome::load_app_icon_texture(ui.ctx(), &mut self.app_icon_texture)
                    });
                    if let Some(event) = chrome::show_sidebar(
                        ui,
                        sidebar_palette,
                        sidebar_content_height(sidebar_rect.height()),
                        SidebarModel {
                            items: &sidebar_items,
                            footer_items: &sidebar_footer_items,
                            session_count: binding_groups
                                .iter()
                                .map(|group| group.sessions.len())
                                .sum(),
                            has_sessions: binding_groups
                                .iter()
                                .any(|group| !group.sessions.is_empty()),
                            title_visible,
                            reserve_titlebar_buttons,
                            title_icon: title_icon.as_ref(),
                            top_inset,
                            border_visible: !fullscreen_chrome,
                            border_bottom: false,
                            separator_visible: false,
                            focused: self.state.sidebar_focused(),
                            hovered_session: self.state.sidebar_hovered_session(),
                            unfocused_dim: self.state.config().chrome.unfocused_sidebar_dim,
                            fullscreen: fullscreen_chrome,
                            hover_override: sidebar_cfg.hover.map(crate::theme::config_color32),
                            current_override: sidebar_cfg
                                .selected
                                .map(crate::theme::config_color32),
                            border_override: sidebar_cfg.border.map(crate::theme::config_color32),
                        },
                    ) {
                        match event {
                            chrome::SidebarEvent::ActivateSession(target) => {
                                self.state.activate_scoped_session_from_ui(&target);
                            }
                            chrome::SidebarEvent::ContextAction { target, action } => {
                                let handled = match action {
                                    chrome::SessionContextAction::Activate => {
                                        self.state.activate_scoped_session_from_ui(&target)
                                    }
                                    chrome::SessionContextAction::PreviousSession => self
                                        .state
                                        .activate_relative_scoped_session_from_ui(&target, -1),
                                    chrome::SessionContextAction::NextSession => self
                                        .state
                                        .activate_relative_scoped_session_from_ui(&target, 1),
                                    action => {
                                        if !self.state.activate_scoped_session_from_ui(&target) {
                                            false
                                        } else {
                                            match action {
                                                chrome::SessionContextAction::NewSession => {
                                                    self.state.open_new_session_dialog_from_ui()
                                                }
                                                chrome::SessionContextAction::SwitchSession => {
                                                    self.state.open_session_picker_dialog_from_ui()
                                                }
                                                chrome::SessionContextAction::LastSession => {
                                                    self.state.activate_last_session_from_ui()
                                                }
                                                chrome::SessionContextAction::Rename => {
                                                    self.state.open_rename_session_dialog_for(
                                                        &target.session_id,
                                                    )
                                                }
                                                chrome::SessionContextAction::MoveUp => self
                                                    .state
                                                    .move_session_from_ui(&target.session_id, -1),
                                                chrome::SessionContextAction::MoveDown => self
                                                    .state
                                                    .move_session_from_ui(&target.session_id, 1),
                                                chrome::SessionContextAction::Detach => self
                                                    .state
                                                    .detach_scoped_session_from_space(&target),
                                                chrome::SessionContextAction::Ditch => {
                                                    self.state.open_ditch_session_dialog_for(
                                                        &target.session_id,
                                                    )
                                                }
                                                chrome::SessionContextAction::Activate
                                                | chrome::SessionContextAction::PreviousSession
                                                | chrome::SessionContextAction::NextSession => {
                                                    unreachable!(
                                                        "scoped session actions handled above"
                                                    )
                                                }
                                            }
                                        }
                                    }
                                };
                                if handled {
                                    ui.ctx().request_repaint();
                                }
                            }
                            chrome::SidebarEvent::Reorder { source, before } => {
                                // Session order is bootty-owned: commit it natively. The republished
                                // mux forces the worker to re-render the sidebar, and that render
                                // reuses cached shell-out results (a reorder changes only order, not
                                // a session's facts), so it lands instantly with correct grouping.
                                if self
                                    .state
                                    .reorder_session_before(&source, before.as_deref())
                                {
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    }
                    if let Some((_, _, progress)) = space_transition {
                        let alpha = ((1.0 - progress) * 180.0) as u8;
                        let content_rect = Rect::from_min_max(
                            sidebar_rect.min,
                            Pos2::new(
                                sidebar_rect.max.x,
                                (sidebar_rect.max.y - space_switcher_height)
                                    .max(sidebar_rect.min.y),
                            ),
                        );
                        ui.painter().rect_filled(
                            content_rect,
                            0.0,
                            egui::Color32::from_rgba_unmultiplied(
                                sidebar_palette.base.r(),
                                sidebar_palette.base.g(),
                                sidebar_palette.base.b(),
                                alpha,
                            ),
                        );
                        ui.ctx().request_repaint();
                    }
                    if let Some(event) = chrome::show_space_switcher(
                        ui,
                        sidebar_palette,
                        &space_items,
                        space_transition,
                    ) {
                        match event {
                            chrome::SpaceSwitcherEvent::Activate(space_id) => {
                                self.state.activate_space_from_ui(space_id);
                            }
                            chrome::SpaceSwitcherEvent::Create => {
                                self.state.open_create_space_dialog_from_ui();
                            }
                            chrome::SpaceSwitcherEvent::Edit(space_id) => {
                                self.state.open_edit_space_dialog_from_ui(space_id);
                            }
                            chrome::SpaceSwitcherEvent::Reconnect(space_id) => {
                                self.state.reconnect_space_from_ui(space_id);
                            }
                            chrome::SpaceSwitcherEvent::Close(space_id) => {
                                self.state.close_space_from_ui(space_id);
                            }
                        }
                        ui.ctx().request_repaint();
                    }
                    if !self.state.sidebar_focused() {
                        let alpha = (self
                            .state
                            .config()
                            .chrome
                            .unfocused_sidebar_dim
                            .clamp(0.0, 1.0)
                            * 255.0)
                            .round() as u8;
                        ui.painter().rect_filled(
                            sidebar_rect,
                            0.0,
                            egui::Color32::from_black_alpha(alpha),
                        );
                    }
                },
            );

            // Drag the inner edge to resize. The handle lives in a foreground layer so it wins the
            // hit-test over the sidebar rows and the terminal beneath the gap.
            if clamped_sidebar_width > 0.0 {
                let handle_x = if sidebar_on_right {
                    sidebar_rect.min.x
                } else {
                    sidebar_rect.max.x
                };
                let handle_rect = Rect::from_center_size(
                    Pos2::new(handle_x, rect.center().y),
                    egui::vec2(SIDEBAR_RESIZE_HANDLE_WIDTH, rect.height()),
                );
                self.state.register_chrome_handle(handle_rect);
                let response = egui::Area::new(egui::Id::new("bootty-sidebar-resize"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(handle_rect.min)
                    .show(ui.ctx(), |ui| {
                        let response = ui.allocate_rect(handle_rect, egui::Sense::drag());
                        if response.hovered() || response.dragged() {
                            ui.painter().line_segment(
                                [
                                    Pos2::new(handle_x, rect.min.y),
                                    Pos2::new(handle_x, rect.max.y),
                                ],
                                egui::Stroke::new(2.0, palette.primary),
                            );
                        }
                        response
                    })
                    .inner;
                if response.hovered() || response.dragged() {
                    ui.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if response.dragged()
                    && let Some(pos) = ui.ctx().pointer_interact_pos()
                {
                    let raw = if sidebar_on_right {
                        rect.max.x - pos.x
                    } else {
                        pos.x - rect.min.x
                    };
                    let max = (rect.width() - MIN_SIDEBAR_WIDTH).max(MIN_SIDEBAR_WIDTH);
                    self.state
                        .set_sidebar_width_live(raw.clamp(MIN_SIDEBAR_WIDTH, max));
                }
                if response.drag_stopped() {
                    let width = self.state.config().chrome.sidebar_width;
                    self.state.persist_sidebar_width(width);
                }
            }
        }

        if top_bar || bottom_bar {
            // Tick once a second so the clock advances and module output refreshes when idle.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            let status_background =
                status_bar_background_color(&chrome_config, palette, notch_chrome_color);

            if top_bar {
                let mut status_event = None;
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(top_status_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        status_event = chrome::show_status_bar(
                            ui,
                            palette,
                            StatusBarModel {
                                segments: &top_segments,
                                tab_context: tab_context.as_ref(),
                                background: status_background,
                                left_padding: status_left_padding,
                                row_height: status_height_config,
                                notch_x: notch_span.map(|(left, right)| left..right),
                                tab_rows: top_tab_row_count,
                                interaction_id: "bootty-top-status-bar-drag",
                            },
                        );
                    },
                );
                self.handle_status_bar_event(ui.ctx(), status_event);
            }

            if bottom_bar {
                let mut status_event = None;
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(bottom_status_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        status_event = chrome::show_status_bar(
                            ui,
                            palette,
                            StatusBarModel {
                                segments: &bottom_segments,
                                tab_context: tab_context.as_ref(),
                                background: status_background,
                                left_padding: status_left_padding,
                                row_height: status_height_config,
                                notch_x: None,
                                tab_rows: bottom_tab_row_count,
                                interaction_id: "bootty-bottom-status-bar-drag",
                            },
                        );
                    },
                );
                self.handle_status_bar_event(ui.ctx(), status_event);
            }
        }

        let terminal_transition_key = self.state.terminal_transition_key();
        // Native-layout backends whose tabs have all been closed have no pane to attach. Paint an
        // empty state instead of the terminal widget, which would otherwise hold the closed
        // terminal's last frame.
        let native_layout_backend =
            backend_uses_native_layout_renderer(self.state.multiplexer_backend());
        let has_terminal = !native_layout_backend
            || self
                .state
                .mux()
                .selected_session_anchor()
                .is_some_and(|anchor| anchor.pane_id.is_some());
        let pane_backing_color = notch_chrome_color.unwrap_or(palette.mantle);
        ui.painter()
            .rect_filled(terminal_rect, 0.0, pane_backing_color);
        if has_terminal {
            self.state.record_pane_area(terminal_rect);
            if native_layout_backend {
                // Native-layout panes render through per-pane widgets keyed by pane id; keep the
                // focused pane's widget in `terminal_widget` so zoom/metrics keep targeting it.
                if self.state.native_multi_pane() {
                    // show_split_panes swaps in the focused widget itself (after click-to-focus).
                    self.show_split_panes(ui, terminal_rect, palette, pane_backing_color);
                    self.show_pane_dividers(ui, terminal_rect, palette, notch_chrome_color);
                } else {
                    if let Some(focused) = self.state.focused_pane() {
                        let widget_key = self.state.pane_widget_key(&focused);
                        self.focus_pane_widget(&widget_key);
                    }
                    // focus_pane_widget swapped the focused pane's widget into terminal_widget;
                    // set the transition key on it so native tab-switches cross-fade like non-native.
                    self.terminal_widget
                        .set_transition_key(terminal_transition_key);
                    let geometry = self.terminal_widget.geometry_for_rect(terminal_rect);
                    if let Err(error) = self
                        .state
                        .resize_native_layout_window(geometry.cols, geometry.rows)
                    {
                        self.state.record_render_error(error);
                    }
                    self.show_single_terminal(
                        ui,
                        terminal_rect,
                        chrome_config.pane_corner_radius,
                        pane_backing_color,
                    );
                }
            } else {
                self.focused_widget_key = None;
                self.terminal_widget
                    .set_transition_key(terminal_transition_key);
                self.show_single_terminal(
                    ui,
                    terminal_rect,
                    chrome_config.pane_corner_radius,
                    pane_backing_color,
                );
            }
            if !self.state.terminal_focused() {
                let dim = self.state.config().chrome.unfocused_terminal_dim;
                ui.painter().rect_filled(
                    terminal_rect,
                    0.0,
                    egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * 255.0) as u8),
                );
            }
        } else {
            self.terminal_widget.reset();
            let painter = ui.painter_at(terminal_rect);
            let color = palette.muted;
            let galley = crate::ui::keycaps::inline_shortcut_galley_from_painter(
                &painter,
                palette,
                crate::ui::keycaps::InlineShortcut {
                    prefix: "No open tabs - press ",
                    trigger: crate::platform::new_tab_shortcut_trigger(),
                    suffix: " to open one",
                },
                color,
                terminal_rect.width(),
                13.0,
            );
            painter.galley(terminal_rect.center() - galley.size() * 0.5, galley, color);
        }
    }

    fn show_new_mux_session_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_dialog() else {
            return;
        };
        let theme = self.state.ui_theme();
        let open_cwds: Vec<String> = self
            .state
            .mux()
            .sessions()
            .iter()
            .filter_map(|session| session.anchor.cwd.clone())
            .collect();
        let event = dialog.show(ctx, theme, &open_cwds);
        self.state.apply_picker_event(dialog, event);
    }

    fn show_space_editor_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_space_editor_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_space_editor_event(dialog, event);
    }

    fn show_session_picker_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_session_picker_dialog() else {
            return;
        };
        let groups = self.state.session_finder_groups();
        let event = dialog.show(ctx, self.state.ui_theme(), &groups);
        self.state.apply_session_picker_event(dialog, event);
    }

    fn show_rename_session_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_rename_session_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_rename_session_event(dialog, event);
    }

    fn show_rename_tab_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_rename_tab_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_rename_tab_event(dialog, event);
    }

    fn show_ditch_session_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_ditch_session_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_ditch_session_event(dialog, event);
    }

    fn show_keybind_help_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_keybind_help_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_keybind_help_event(dialog, event);
    }

    fn show_command_palette_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_command_palette_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        let run = matches!(
            event,
            crate::ui::command_palette::CommandPaletteEvent::Run(_)
        );
        self.state.apply_command_palette_event(dialog, event);
        // The chosen command runs on the next input pass; make sure that pass
        // happens even if no further input arrives.
        if run {
            ctx.request_repaint();
        }
    }

    fn show_terminal_find_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_terminal_find_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        let searched = matches!(
            event,
            crate::ui::terminal_find::TerminalFindEvent::Search { .. }
        );
        self.state.apply_terminal_find_event(dialog, event);
        if searched {
            ctx.request_repaint();
        }
    }

    fn show_theme_picker_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_theme_picker_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        let mut effects = Vec::new();
        self.state
            .apply_theme_picker_event(dialog, event, &mut effects);
        self.apply_effects(ctx, effects);
    }

    /// Drain Luau `bootty.window` requests from both workers, render the active
    /// window through the overlay framework, and route the user's choice back to
    /// the owning worker's `on_action` handler.
    fn drive_lua_windows(&mut self, ctx: &egui::Context) {
        use crate::extensions::WindowRequest;
        use crate::ui::lua_window::{LuaWindowDialog, LuaWindowEvent};

        // Drain both hosts every frame so a programmatic close() or a replacement
        // open() is honored even while a window is already up. A window that goes
        // away without a user choice notifies its owner to drop its handler.
        for (owner, requests) in [
            (
                LuaWindowOwner::Sidebar,
                self.sidebar_extensions.take_window_requests(),
            ),
            (
                LuaWindowOwner::Status,
                self.status_extensions.take_window_requests(),
            ),
        ] {
            for request in requests {
                match request {
                    WindowRequest::Open(spec) => {
                        self.dismiss_lua_window();
                        self.lua_window = Some((owner, LuaWindowDialog::new(spec)));
                    }
                    WindowRequest::Close => self.dismiss_lua_window(),
                }
            }
        }

        let Some((owner, mut dialog)) = self.lua_window.take() else {
            return;
        };
        match dialog.show(ctx, self.state.ui_theme()) {
            LuaWindowEvent::None => self.lua_window = Some((owner, dialog)),
            LuaWindowEvent::Close => self.lua_host(owner).close_window(dialog.id()),
            LuaWindowEvent::Action { key, value } => {
                self.lua_host(owner)
                    .push_window_action(dialog.id(), key, value);
            }
        }
    }

    /// Close any open Luau window and tell its owning worker to drop the handler.
    fn dismiss_lua_window(&mut self) {
        if let Some((owner, dialog)) = self.lua_window.take() {
            self.lua_host(owner).close_window(dialog.id());
        }
    }

    fn lua_host(&self, owner: LuaWindowOwner) -> &crate::extensions::ExtensionHost {
        match owner {
            LuaWindowOwner::Sidebar => &self.sidebar_extensions,
            LuaWindowOwner::Status => &self.status_extensions,
        }
    }
}

fn effective_session_modules(
    sidebar: &crate::config::SidebarConfig,
    legacy_sessions_override: bool,
) -> &[String] {
    if legacy_sessions_override && !sidebar.session_modules_configured {
        &[]
    } else {
        &sidebar.session_modules
    }
}

pub(crate) fn compose_session_module_items(
    base: Vec<crate::extensions::ModuleItem>,
    components: impl IntoIterator<Item = crate::extensions::ModuleItem>,
) -> Vec<crate::extensions::ModuleItem> {
    let mut overlays = HashMap::<String, Vec<crate::extensions::ModulePrimitive>>::new();
    let mut rows = HashMap::<String, Vec<crate::extensions::ModuleItem>>::new();
    let mut unscoped = Vec::new();
    for item in components {
        let Some(session_id) = item.session_id.clone() else {
            unscoped.push(item);
            continue;
        };
        if item.kind.as_deref() == Some("session-overlay") {
            overlays
                .entry(session_id)
                .or_default()
                .extend(item.primitives);
        } else {
            rows.entry(session_id).or_default().push(item);
        }
    }

    let mut composed = Vec::with_capacity(base.len() + rows.values().map(Vec::len).sum::<usize>());
    for mut item in base {
        let session_id = item
            .kind
            .as_deref()
            .filter(|kind| *kind == "session")
            .and(item.session_id.clone());
        if let Some(session_id) = session_id {
            if let Some(primitives) = overlays.remove(&session_id) {
                item.primitives.extend(primitives);
            }
            composed.push(item);
            if let Some(mut session_rows) = rows.remove(&session_id) {
                composed.append(&mut session_rows);
            }
        } else {
            composed.push(item);
        }
    }
    composed.extend(unscoped);
    composed
}

fn suppress_settings_recorder_duplicates(
    events: &mut Vec<egui::Event>,
    direct_inputs: &[crate::direct_input::DirectKeyInput],
    recording: bool,
) {
    if recording {
        suppress_egui_events_for_direct_input(events, direct_inputs);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ErrorToastText {
    summary: String,
    details: Option<String>,
}

fn error_toast_text(error: &str) -> ErrorToastText {
    let normalized = error.trim();
    let lower = normalized.to_ascii_lowercase();
    let summary = if lower.contains("rmux") {
        "Could not reach remote rmux.".to_owned()
    } else if lower.contains("ssh") || lower.contains("connection") {
        "Could not reach the remote workspace.".to_owned()
    } else {
        let first_line = normalized.lines().next().unwrap_or("Operation failed.");
        if first_line.chars().count() <= 96 {
            first_line.to_owned()
        } else {
            "The operation failed. Open details for the technical error.".to_owned()
        }
    };
    let details = (summary != normalized).then(|| normalized.to_owned());
    ErrorToastText { summary, details }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.state.drain_direct_input();
        self.state.set_lua_window_open(self.lua_window.is_some());
        if self.settings_open {
            suppress_settings_recorder_duplicates(
                &mut raw_input.events,
                self.state.pending_direct_input(),
                self.settings.is_recording_keybind(),
            );
            return;
        }
        if self.state.direct_input_suppresses_egui_events() {
            suppress_egui_events_for_direct_input(
                &mut raw_input.events,
                self.state.pending_direct_input(),
            );
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.window_focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let (
            mut events,
            mut dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            stable_dt,
            viewport,
            zoom_delta,
        ) = ctx.input(|input| {
            (
                input.events.clone(),
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect::<Vec<PathBuf>>(),
                input.modifiers,
                input.pointer.hover_pos(),
                crate::input::pressed_mouse_button_from_egui(&input.pointer),
                input.stable_dt,
                ViewportSnapshot {
                    fullscreen: input.viewport().fullscreen.unwrap_or(false),
                    maximized: input.viewport().maximized.unwrap_or(false),
                    content_height: input.content_rect().height(),
                },
                input.zoom_delta(),
            )
        });
        if self.settings_open {
            suppress_terminal_payload_for_settings(&mut events, &mut dropped_file_paths);
        }

        let (terminal_cell_width, terminal_cell_height) = self.terminal_widget.cell_dimensions();

        if !self.settings_open && (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.terminal_widget.apply_pinch(zoom_delta, hover_pos);
        }
        if !self.settings_open && self.terminal_widget.is_zoomed() {
            let pan = take_scroll_for_pan(&mut events, terminal_cell_height);
            if pan != Vec2::ZERO {
                self.terminal_widget.apply_pan(pan);
            }
        }

        let now = Instant::now();
        self._command_extensions.refresh(now);
        let inputs = FrameInputs {
            now,
            stable_dt_ms: stable_dt * 1000.0,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            window_focused: self.window_focused,
            renderer_metrics: self.terminal_widget.metrics(),
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor: ctx.pixels_per_point(),
            terminal_view_transform: self.terminal_widget.view_transform(),
        };
        let effects = self.state.update_frame(inputs);
        self.apply_effects(ctx, effects);
        if animate_indeterminate_progress(
            self.window_focused,
            self.state.has_indeterminate_terminal_progress(),
        ) {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        if crate::menu::settings_requested() {
            self.open_settings(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let system_variant = match ui.ctx().system_theme().unwrap_or(egui::Theme::Dark) {
            egui::Theme::Light => AppearanceVariant::Light,
            egui::Theme::Dark => AppearanceVariant::Dark,
        };
        let variant = self.state.config().appearance.mode.variant(system_variant);
        self.state.set_appearance_variant(variant);
        self.sync_extension_theme(ui.ctx());
        let theme = self.state.ui_theme();
        sync_global_egui_style(ui.ctx(), theme);
        let palette = theme.palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            if self.settings_open {
                self.show_settings(ui);
            } else {
                self.show_fixed_layout(ui);
            }
        });
        if let Some(error) = self.state.last_error().map(str::to_owned) {
            let toast = error_toast_text(&error);
            let mut dismiss = false;
            egui::Area::new(egui::Id::new("last-error"))
                .order(egui::Order::Tooltip)
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -12.0])
                .show(ui.ctx(), |ui| {
                    let max_width = (ui.ctx().content_rect().width() - 48.0).clamp(280.0, 560.0);
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(max_width);
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&toast.summary).color(palette.destructive),
                                )
                                .wrap(),
                            );
                            ui.horizontal(|ui| {
                                if toast.details.is_some()
                                    && ui
                                        .button(if self.error_details_open {
                                            "Hide details"
                                        } else {
                                            "Details"
                                        })
                                        .clicked()
                                {
                                    self.error_details_open = !self.error_details_open;
                                }
                                dismiss = ui.button("Dismiss").clicked();
                            });
                            if self.error_details_open
                                && let Some(details) = &toast.details
                            {
                                egui::ScrollArea::vertical()
                                    .max_height(180.0)
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(details)
                                                    .monospace()
                                                    .size(11.0)
                                                    .color(palette.subtext),
                                            )
                                            .wrap(),
                                        );
                                    });
                            }
                        });
                    });
                });
            if dismiss {
                self.error_details_open = false;
                self.state.clear_last_error();
            }
        }
        if !self.settings_open {
            self.show_new_mux_session_dialog(ui.ctx());
            self.show_space_editor_dialog(ui.ctx());
            self.show_session_picker_dialog(ui.ctx());
            self.show_rename_session_dialog(ui.ctx());
            self.show_rename_tab_dialog(ui.ctx());
            self.show_ditch_session_dialog(ui.ctx());
            self.show_keybind_help_dialog(ui.ctx());
            self.show_command_palette_dialog(ui.ctx());
            self.show_terminal_find_dialog(ui.ctx());
            self.show_theme_picker_dialog(ui.ctx());
            self.drive_lua_windows(ui.ctx());
        }
        let cursor_icon = ui.ctx().output(|output| output.cursor_icon);
        crate::platform::set_macos_cursor_icon(cursor_icon);
    }
}

fn uses_custom_egui_fonts(config: &BoottyConfig) -> bool {
    config.chrome.sidebar || config.chrome.top_bar || config.chrome.bottom_bar
}

fn suppress_terminal_payload_for_settings(
    events: &mut Vec<egui::Event>,
    dropped_file_paths: &mut Vec<PathBuf>,
) {
    events.clear();
    dropped_file_paths.clear();
}

// egui's wheel `delta` already points the way content should move, so it is the pan delta as-is.
fn take_scroll_for_pan(events: &mut Vec<egui::Event>, line_height: f32) -> Vec2 {
    let mut pan = Vec2::ZERO;
    events.retain(|event| {
        let egui::Event::MouseWheel {
            unit,
            delta,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        if modifiers.alt {
            return true;
        }
        let scale = match unit {
            egui::MouseWheelUnit::Point => 1.0,
            egui::MouseWheelUnit::Line => line_height,
            egui::MouseWheelUnit::Page => line_height * 20.0,
        };
        pan += *delta * scale;
        false
    });
    pan
}

fn configure_egui_fonts(ctx: &egui::Context, families: &[String]) {
    let db = bootty_render::font_database::system_font_database();
    let mut fonts = FontDefinitions::default();
    let mut loaded_text_font = false;
    for family in families.iter().rev() {
        loaded_text_font |= add_egui_font_family(&mut fonts, db, family, EguiFontPlacement::First);
    }
    if !loaded_text_font {
        add_egui_default_text_font(&mut fonts, db);
    }
    add_egui_symbol_fallback_fonts(&mut fonts, db);
    crate::ui::icons::add_icon_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

#[derive(Clone, Copy)]
enum EguiFontPlacement {
    First,
    Last,
}

fn add_egui_default_text_font(fonts: &mut FontDefinitions, db: &fontdb::Database) -> bool {
    let query_families = [fontdb::Family::Monospace];
    let query = fontdb::Query {
        families: &query_families,
        ..fontdb::Query::default()
    };
    let Some(id) = db.query(&query) else {
        return false;
    };
    add_egui_font_face(
        fonts,
        db,
        id,
        "bootty-ui-default-monospace",
        EguiFontPlacement::First,
    )
}

fn add_egui_symbol_fallback_fonts(fonts: &mut FontDefinitions, db: &fontdb::Database) {
    for family in EGUI_SYMBOL_FALLBACK_FAMILIES {
        if add_egui_font_family(fonts, db, family, EguiFontPlacement::Last) {
            break;
        }
    }
    for ch in EGUI_SYMBOL_FALLBACK_CHARS {
        add_egui_font_for_char(fonts, db, *ch);
    }
}

fn add_egui_font_family(
    fonts: &mut FontDefinitions,
    db: &fontdb::Database,
    family: &str,
    placement: EguiFontPlacement,
) -> bool {
    let name = egui_font_name(family);
    let query_families = [fontdb::Family::Name(family)];
    let query = fontdb::Query {
        families: &query_families,
        ..fontdb::Query::default()
    };
    let Some(id) = db.query(&query) else {
        return false;
    };
    add_egui_font_face(fonts, db, id, &name, placement)
}

fn add_egui_font_for_char(fonts: &mut FontDefinitions, db: &fontdb::Database, ch: char) -> bool {
    let Some(face) = egui_symbol_fallback_face(db, ch) else {
        return false;
    };
    let name = format!(
        "bootty-ui-symbol-U{:04X}-{}",
        u32::from(ch),
        face.post_script_name
    );
    add_egui_font_face(fonts, db, face.id, &name, EguiFontPlacement::Last)
}

fn add_egui_font_face(
    fonts: &mut FontDefinitions,
    db: &fontdb::Database,
    id: fontdb::ID,
    name: &str,
    placement: EguiFontPlacement,
) -> bool {
    let name = db
        .face(id)
        .map(|face| format!("bootty-ui-face-{}", face.post_script_name))
        .unwrap_or_else(|| name.to_owned());
    if fonts.font_data.contains_key(&name) {
        return false;
    }
    let Some((bytes, index)) = db.with_face_data(id, |data, index| (data.to_vec(), index)) else {
        return false;
    };

    let mut font_data = FontData::from_owned(bytes);
    font_data.index = index;
    fonts
        .font_data
        .insert(name.clone(), std::sync::Arc::new(font_data));
    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        let entries = fonts.families.entry(family).or_default();
        match placement {
            EguiFontPlacement::First => entries.insert(0, name.clone()),
            EguiFontPlacement::Last => entries.push(name.clone()),
        }
    }
    true
}

fn egui_symbol_fallback_face(db: &fontdb::Database, ch: char) -> Option<&fontdb::FaceInfo> {
    let mut fallback = None;
    for face in db.faces() {
        if !font_face_supports_char(db, face.id, ch) {
            continue;
        }
        if face.monospaced {
            return Some(face);
        }
        fallback.get_or_insert(face);
    }
    fallback
}

fn font_face_supports_char(db: &fontdb::Database, id: fontdb::ID, ch: char) -> bool {
    db.with_face_data(id, |data, index| {
        ttf_parser::Face::parse(data, index).is_ok_and(|face| face.glyph_index(ch).is_some())
    })
    .unwrap_or(false)
}

fn egui_font_name(family: &str) -> String {
    format!("bootty-ui-{family}")
}
