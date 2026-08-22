use std::collections::HashMap;

use eframe::egui::{self, Pos2, Rect, TextureHandle, UiBuilder};

use crate::{
    state::AppState,
    theme::{module_color32, theme_palette_from_config},
    ui::chrome::{self, SidebarModel, StatusBarModel},
};
use bootty_extension::{
    ExtensionHost, ModulePrimitive, PublishedSurfaceItem, PublishedSurfaceSnapshot,
    SurfacePlacement,
};
use bootty_workspace::DEFAULT_SPACE_COLOR;

/// Fallback layout offset in points when the active screen has a notch but macOS does not report
/// its band.
const FALLBACK_NOTCH_LAYOUT_OFFSET: f32 = 24.0;
const MACOS_NOTCH_MENU_BAR_OVERSHOOT: f32 = 7.0;
const FULLSCREEN_NOTCH_TAB_ROW_CLEARANCE: f32 = 4.0;
const SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 8.0;

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

fn when<T: Default>(enabled: bool, value: impl FnOnce() -> T) -> T {
    if enabled { value() } else { T::default() }
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

#[derive(Default)]
pub(crate) struct ChromeRuntime {
    app_icon_texture: Option<TextureHandle>,
    sidebar_space_swipe: chrome::SidebarSpaceSwipeState,
}

impl ChromeRuntime {
    /// Paint the chrome for one frame. Leaf interactions come back as their existing event types
    /// for the owner to run; the terminal rect and colors come back so the shell paints the
    /// terminal after those events have been applied.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        extensions: &ExtensionHost,
        tab_context: Option<&chrome::TabContext>,
        terminal_cell_height: f32,
    ) -> ChromeFrame {
        ChromeView {
            chrome: self,
            state,
            extensions,
            tab_context,
            terminal_cell_height,
        }
        .show_fixed_layout(ui)
    }
}

struct ChromeView<'a> {
    chrome: &'a mut ChromeRuntime,
    /// Read-only: every mutation this frame implies travels back as a [`ChromeEvents`] entry for
    /// the owner to run.
    state: &'a AppState,
    /// Read-only: the frame's published surfaces. Submissions are owner work and happen after
    /// this pass, from the collected leaf events.
    extensions: &'a ExtensionHost,
    /// Borrowed from the owner's projection for this frame.
    tab_context: Option<&'a chrome::TabContext>,
    terminal_cell_height: f32,
}

/// What the chrome pass produced: the interactions still to be applied, and where the terminal
/// goes once they have been.
pub(crate) struct ChromeFrame {
    pub(crate) events: ChromeEvents,
    /// Interactive chrome rects painted this frame; the owner installs them for the next input
    /// pass, and the terminal pass appends its pane dividers.
    pub(crate) handles: Vec<Rect>,
    pub(crate) terminal: TerminalPaint,
}

/// Leaf events collected while chrome painted, in the order the owner must replay them.
#[derive(Default)]
pub(crate) struct ChromeEvents {
    /// A trackpad swipe across the sidebar. Applied before a switcher click from the same frame,
    /// matching the paint order: the swipe is read before the switcher is drawn.
    pub(crate) swipe_space: Option<bootty_mux::controller::SpaceId>,
    pub(crate) sidebar: Option<chrome::SidebarEvent>,
    pub(crate) spaces: Option<chrome::SpaceSwitcherEvent>,
    pub(crate) resize: Option<SidebarResize>,
    /// One per painted bar, top before bottom.
    pub(crate) status: Vec<chrome::StatusBarEvent>,
}

/// The sidebar resize drag: a live width while dragging, one config write on release.
pub(crate) enum SidebarResize {
    Live(f32),
    Persist,
}

pub(crate) struct TerminalPaint {
    pub(crate) rect: Rect,
    pub(crate) palette: bootty_ui::ThemePalette,
    pub(crate) pane_backing_color: egui::Color32,
    pub(crate) notch_chrome_color: Option<egui::Color32>,
}

impl ChromeView<'_> {
    fn resolve_status_segments<'a>(
        segments: &'a [bootty_config::config::StatusSegment],
        surfaces: &'a [PublishedSurfaceSnapshot],
    ) -> Vec<chrome::ResolvedSegment<'a>> {
        segments
            .iter()
            .enumerate()
            .filter_map(|(source_slot, segment)| {
                let seg_fg = segment.fg.map(crate::theme::config_color32);
                let seg_bg = segment.bg.map(crate::theme::config_color32);
                let surface = published_surface(surfaces, &segment.module)?;
                let items = surface
                    .snapshot
                    .items
                    .iter()
                    .map(|item| chrome::ResolvedItem {
                        item,
                        icon: item.icon.as_deref().or(segment.icon.as_deref()),
                        stroke: item.stroke.map(module_color32),
                        fg: item.fg.map(module_color32).or(seg_fg),
                        bg: item.bg.map(module_color32).or(seg_bg),
                    })
                    .collect::<Vec<_>>();
                (!items.is_empty()).then_some(chrome::ResolvedSegment {
                    align: segment.align,
                    source_slot,
                    module: &surface.module,
                    generation: surface.generation,
                    surface: &surface.snapshot.declaration.id,
                    items,
                })
            })
            .collect()
    }

    fn show_fixed_layout(&mut self, ui: &mut egui::Ui) -> ChromeFrame {
        let mut events = ChromeEvents::default();
        // Collected, not registered: the owner installs this frame's set once chrome has painted,
        // so the next input pass suppresses selection over live handles only.
        let mut handles = Vec::new();
        let rect = ui.max_rect();
        let palette =
            theme_palette_from_config(self.state.config(), self.state.active_appearance_variant());
        let chrome_config = self.state.config().chrome.clone();
        let sidebar = chrome_config.sidebar;
        let top_bar = chrome_config.top_bar;
        let bottom_bar = chrome_config.bottom_bar;
        let configured_sidebar_width = chrome_config.sidebar_width;
        let status_height_config = chrome_config.status_height;
        let chrome_gap = chrome_config.gap;
        // Window and notch facts were sampled by the owner before this pass. Reserve a top offset
        // in fullscreen to clear the notch: the explicit override applies whenever fullscreen so it
        // works even when auto-detection can't read the notch (a hidden menu bar zeroes
        // safeAreaInsets); the measured value only fills in when unset.
        let window_chrome = self.state.window_chrome_facts();
        let fullscreen_chrome = window_chrome.fullscreen;
        let notch_context = window_chrome.notched;
        let black_notch_chrome = notch_context
            && chrome_config.notched_fullscreen_black_chrome
            && self.state.active_appearance_variant()
                == bootty_config::config::AppearanceVariant::Dark;
        let notch_chrome_color = black_notch_chrome.then_some(egui::Color32::BLACK);
        // Pixel height for the layout offset: the config override, else the measured macOS band
        // calibrated to the physical notch, else a fallback when the band is unreadable.
        let measured_band = window_chrome.notch_band;
        let fullscreen_top_offset = when(notch_context, || {
            fullscreen_notch_layout_offset(
                self.state.config().window.fullscreen_top_offset,
                measured_band,
            )
        });
        // When enabled, the terminal/tab bar sits inside the notch band instead of being pushed
        // entirely below it.
        let tabs_in_notch = notch_context && self.state.config().window.fullscreen_tabs_in_notch;
        let notch_band_color = notch_chrome_color.unwrap_or(palette.base);
        let sidebar_width = when(sidebar, || configured_sidebar_width);
        let gap = when(sidebar_width > 0.0 && !fullscreen_chrome, || chrome_gap);
        let sidebar_on_right = matches!(
            self.state.config().sidebar.position,
            bootty_config::config::SidebarPosition::Right
        );
        let clamped_sidebar_width = sidebar_width.min(rect.width());
        let (sidebar_rect, right_rect) = if sidebar_on_right {
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
        let top_bar_left_inset = when(
            (!sidebar || sidebar_on_right)
                && self
                    .state
                    .config()
                    .window
                    .reserves_macos_titlebar_button_area(),
            || chrome::MACOS_TITLEBAR_BUTTON_SAFE_WIDTH,
        );
        let status_left_padding = chrome::STATUS_EDGE_PAD;
        let status_surfaces = when(top_bar || bottom_bar, || {
            self.extensions.surfaces(SurfacePlacement::Status)
        });
        let top_segments = when(top_bar, || {
            Self::resolve_status_segments(&chrome_config.top_segments, &status_surfaces)
        });
        let bottom_segments = when(bottom_bar, || {
            Self::resolve_status_segments(&chrome_config.bottom_segments, &status_surfaces)
        });
        let top_base_status_height = when(top_bar, || status_height_config);
        let bottom_base_status_height = when(bottom_bar, || status_height_config);
        let notch_span = tabs_in_notch
            .then_some(window_chrome.notch_span)
            .flatten()
            .map(|(left, right)| (rect.min.x + left, rect.min.x + right));
        let candidate_top_status_rect = Rect::from_min_max(
            Pos2::new(
                (right_rect.min.x + top_bar_left_inset).min(right_rect.max.x),
                right_rect.min.y,
            ),
            Pos2::new(right_rect.max.x, right_rect.min.y + top_base_status_height),
        );
        let candidate_bottom_status_rect = Rect::from_min_size(
            right_rect.min,
            egui::vec2(right_rect.width(), bottom_base_status_height),
        );
        let [top_status_layout, bottom_status_layout] = [
            (
                top_bar,
                candidate_top_status_rect,
                top_segments.as_slice(),
                notch_span,
            ),
            (
                bottom_bar,
                candidate_bottom_status_rect,
                bottom_segments.as_slice(),
                None,
            ),
        ]
        .map(|(visible, rect, segments, notch)| {
            visible
                .then(|| chrome::status_bar_layout(ui, rect, segments, status_left_padding, notch))
        });
        let top_tab_row_count = top_status_layout
            .as_ref()
            .map_or(1, chrome::StatusBarLayout::row_count);
        let bottom_tab_row_count = bottom_status_layout
            .as_ref()
            .map_or(1, chrome::StatusBarLayout::row_count);
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
        let terminal_cell_height = self.terminal_cell_height;
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
            let sidebar_cfg = self.state.config().sidebar.clone();
            let sidebar_surfaces = self.extensions.surfaces(SurfacePlacement::Sidebar);
            let session_surfaces = self.extensions.surfaces(SurfacePlacement::Session);
            let (sidebar_footer_items, sidebar_module_items): (Vec<_>, Vec<_>) = sidebar_cfg
                .modules
                .iter()
                .flat_map(|name| published_surface_items(&sidebar_surfaces, name))
                .partition(|item| item.item.kind.as_deref() == Some("footer"));
            let sidebar_module_items = compose_session_module_items(
                sidebar_module_items,
                sidebar_cfg
                    .session_modules
                    .iter()
                    .flat_map(|name| published_surface_items(&session_surfaces, name)),
            );
            let space_items = self.state.space_summaries();
            let active_space_appearance = space_items
                .iter()
                .find(|space| space.active)
                .map(|space| (space.color, space.tint_sidebar))
                .unwrap_or((DEFAULT_SPACE_COLOR, false));
            let space_transition = self.state.space_transition(std::time::Instant::now());
            let binding_groups =
                (self.state.binding_count() > 1).then(|| self.state.binding_session_groups());
            let sidebar_items = if let Some(groups) = &binding_groups {
                crate::ui::sidebar::build_binding_sidebar_items(groups)
            } else {
                crate::ui::sidebar::build_sidebar_items_from_published_items(
                    &sidebar_module_items,
                    self.state.mux_scope(),
                    self.state.mux().selected_session(),
                    self.state.mux().previous_selected_session().is_some(),
                )
            };
            let session_count = binding_groups.as_ref().map_or_else(
                || self.state.mux().sessions().len(),
                |groups| groups.iter().map(|group| group.sessions.len()).sum(),
            );
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
                    events.swipe_space = chrome::take_sidebar_space_swipe(
                        ui,
                        sidebar_rect,
                        &space_items,
                        &mut self.chrome.sidebar_space_swipe,
                    );
                    let title_icon = title_visible.then(|| {
                        chrome::load_app_icon_texture(ui.ctx(), &mut self.chrome.app_icon_texture)
                    });
                    let event = chrome::show_sidebar(
                        ui,
                        sidebar_palette,
                        (sidebar_rect.height() - chrome::SPACE_SWITCHER_HEIGHT).max(0.0),
                        SidebarModel {
                            items: &sidebar_items,
                            footer_items: &sidebar_footer_items,
                            session_count,
                            title_visible,
                            reserve_titlebar_buttons,
                            title_icon: title_icon.as_ref(),
                            top_inset,
                            border_visible: !fullscreen_chrome,
                            border_bottom: false,
                            separator_visible: false,
                            focused: self.state.sidebar_focused(),
                            hovered_session: self.state.sidebar_hovered_session(),
                            fullscreen: fullscreen_chrome,
                            hover_override: sidebar_cfg.hover.map(crate::theme::config_color32),
                            current_override: sidebar_cfg
                                .selected
                                .map(crate::theme::config_color32),
                            border_override: sidebar_cfg.border.map(crate::theme::config_color32),
                        },
                    );
                    events.sidebar = event;
                    if let Some((_, _, progress)) = space_transition {
                        let alpha = ((1.0 - progress) * 180.0) as u8;
                        let content_rect = Rect::from_min_max(
                            sidebar_rect.min,
                            Pos2::new(
                                sidebar_rect.max.x,
                                (sidebar_rect.max.y - chrome::SPACE_SWITCHER_HEIGHT)
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
                    events.spaces = chrome::show_space_switcher(
                        ui,
                        sidebar_palette,
                        &space_items,
                        space_transition,
                    );
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
                handles.push(handle_rect);
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
                    let max = (rect.width() - 120.0).max(120.0);
                    events.resize = Some(SidebarResize::Live(raw.clamp(120.0, max)));
                }
                if response.drag_stopped() {
                    events.resize = Some(SidebarResize::Persist);
                }
            }
        }

        if top_bar || bottom_bar {
            // Tick once a second so the clock advances and module output refreshes when idle.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            let status_background = notch_chrome_color
                .or_else(|| {
                    chrome_config
                        .status_background
                        .map(crate::theme::config_color32)
                })
                .unwrap_or(palette.mantle);

            for (rect, layout, interaction_id) in [
                (
                    top_status_rect,
                    top_status_layout.as_ref(),
                    "bootty-top-status-bar-drag",
                ),
                (
                    bottom_status_rect,
                    bottom_status_layout.as_ref(),
                    "bootty-bottom-status-bar-drag",
                ),
            ] {
                let Some(layout) = layout else {
                    continue;
                };
                let mut status_event = None;
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        status_event = chrome::show_status_bar(
                            ui,
                            palette,
                            StatusBarModel {
                                layout,
                                tab_context: self.tab_context,
                                background: status_background,
                                row_height: status_height_config,
                                interaction_id,
                            },
                        );
                    },
                );
                events.status.extend(status_event);
            }
        }

        ChromeFrame {
            events,
            handles,
            terminal: TerminalPaint {
                rect: terminal_rect,
                palette,
                pane_backing_color: notch_chrome_color.unwrap_or(palette.mantle),
                notch_chrome_color,
            },
        }
    }
}

fn published_surface<'a>(
    surfaces: &'a [PublishedSurfaceSnapshot],
    name: &str,
) -> Option<&'a PublishedSurfaceSnapshot> {
    surfaces.iter().find(|surface| surface.matches_name(name))
}

fn published_surface_items<'a>(
    surfaces: &'a [PublishedSurfaceSnapshot],
    name: &str,
) -> impl Iterator<Item = PublishedSurfaceItem> + 'a {
    published_surface(surfaces, name)
        .into_iter()
        .flat_map(|surface| surface.items())
}

fn compose_session_module_items(
    base: Vec<PublishedSurfaceItem>,
    components: impl IntoIterator<Item = PublishedSurfaceItem>,
) -> Vec<PublishedSurfaceItem> {
    let mut overlays = HashMap::<String, Vec<ModulePrimitive>>::new();
    let mut rows = HashMap::<String, Vec<PublishedSurfaceItem>>::new();
    let mut unscoped = Vec::new();
    for item in components {
        let Some(session_id) = item.item.session_id.clone() else {
            unscoped.push(item);
            continue;
        };
        if item.item.kind.as_deref() == Some("session-overlay") {
            overlays
                .entry(session_id)
                .or_default()
                .extend(item.item.primitives);
        } else {
            rows.entry(session_id).or_default().push(item);
        }
    }

    let mut composed = Vec::with_capacity(base.len() + rows.values().map(Vec::len).sum::<usize>());
    for mut item in base {
        let session_id = item
            .item
            .kind
            .as_deref()
            .filter(|kind| *kind == "session")
            .and(item.item.session_id.clone());
        if let Some(session_id) = session_id {
            if let Some(primitives) = overlays.remove(&session_id) {
                item.item.primitives.extend(primitives);
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
