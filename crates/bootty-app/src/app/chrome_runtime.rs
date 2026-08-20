use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Pos2, Rect, TextureHandle, UiBuilder};

use super::{
    binding_terminal_facts::TerminalProgressState, state::AppState,
    terminal_workspace_view::TerminalWorkspaceView,
};
use crate::{
    theme::theme_palette_from_config,
    ui::chrome::{self, SidebarModel, StatusBarModel},
};
use bootty_extension::{
    ExtensionHost, ExtensionUiAction, ModuleColor, ModulePrimitive, MuxView, PublishedSurfaceItem,
    PublishedSurfaceSnapshot, SessionProgressView, SessionView, SurfacePlacement, WindowView,
};

fn module_color(value: ModuleColor) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(value.r, value.g, value.b, value.a)
}

/// Fallback layout offset in points when the active screen has a notch but macOS does not report
/// its band.
const FALLBACK_NOTCH_LAYOUT_OFFSET: f32 = 24.0;
const MACOS_NOTCH_MENU_BAR_OVERSHOOT: f32 = 7.0;
const FULLSCREEN_NOTCH_TAB_ROW_CLEARANCE: f32 = 4.0;
const SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 8.0;

fn sidebar_content_height(sidebar_height: f32) -> f32 {
    (sidebar_height - chrome::SPACE_SWITCHER_HEIGHT).max(0.0)
}

fn color_hex(color: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
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

#[derive(Default)]
pub(super) struct ChromeRuntime {
    app_icon_texture: Option<TextureHandle>,
    keep_awake: Option<keepawake::KeepAwake>,
    sidebar_space_swipe: chrome::SidebarSpaceSwipeState,
}

impl ChromeRuntime {
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        extensions: &mut ExtensionHost,
        terminal_view: &mut TerminalWorkspaceView,
        window_focused: bool,
    ) {
        ChromeView {
            chrome: self,
            state,
            extensions,
            terminal_view,
            window_focused,
        }
        .show_fixed_layout(ui);
    }
}

struct ChromeView<'a> {
    chrome: &'a mut ChromeRuntime,
    state: &'a mut AppState,
    extensions: &'a mut ExtensionHost,
    terminal_view: &'a mut TerminalWorkspaceView,
    window_focused: bool,
}

impl ChromeView<'_> {
    fn resolve_status_segments(
        &self,
        segments: &[crate::config::StatusSegment],
    ) -> Vec<chrome::ResolvedSegment> {
        segments
            .iter()
            .enumerate()
            .filter_map(|(source_slot, segment)| {
                let seg_fg = segment.fg.map(crate::theme::config_color32);
                let seg_bg = segment.bg.map(crate::theme::config_color32);
                let surface = self
                    .extensions
                    .surface(SurfacePlacement::Status, &segment.module);
                let items =
                    surface
                        .into_iter()
                        .flat_map(|surface| {
                            let module = surface.module;
                            let generation = surface.generation;
                            let surface_id = surface.snapshot.declaration.id;
                            surface.snapshot.items.into_iter().map(move |item| {
                                (item, module.clone(), generation, surface_id.clone())
                            })
                        })
                        .map(|(item, module, generation, surface)| chrome::ResolvedItem {
                            text: item.text,
                            icon: item.icon.or_else(|| segment.icon.clone()),
                            stroke: item.stroke.map(module_color),
                            fg: item.fg.map(module_color).or(seg_fg),
                            bg: item.bg.map(module_color).or(seg_bg),
                            gauge: item.gauge,
                            primitives: item.primitives,
                            pad_left: item.pad_left,
                            pad_right: item.pad_right,
                            join: item.join,
                            gap: item.gap,
                            action: item.action,
                            reorder_anchor: item.reorder_anchor,
                            module,
                            generation,
                            surface,
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

    fn current_extension_mux_view(&self, sidebar_visible: bool) -> MuxView {
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
                WindowView {
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
        MuxView {
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
            keep_awake: self.chrome.keep_awake.is_some(),
            focused: self.window_focused,
        }
    }

    fn current_status_tab_context(&self) -> Option<chrome::TabContext> {
        let selected_session = self.state.mux().selected_session()?;
        let session = self.state.mux().session_by_id_or_name(selected_session)?;
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

    fn current_extension_sessions(&self) -> Vec<SessionView> {
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
                            progresses.push(SessionProgressView {
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
                SessionView {
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
    fn publish_extension_mux_view(&self, sidebar_visible: bool) {
        let view = self.current_extension_mux_view(sidebar_visible);
        self.extensions.update_mux(view);
    }

    fn toggle_keep_awake(&mut self) {
        if self.chrome.keep_awake.take().is_some() {
            self.publish_extension_mux_view(self.state.config().chrome.sidebar);
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
            Ok(guard) => self.chrome.keep_awake = Some(guard),
            Err(error) => self.state.record_render_error(error),
        }
        self.publish_extension_mux_view(self.state.config().chrome.sidebar);
    }
    fn handle_status_bar_event(
        &mut self,
        ctx: &egui::Context,
        status_event: Option<chrome::StatusBarEvent>,
    ) {
        match status_event {
            Some(chrome::StatusBarEvent::Action {
                module,
                generation,
                surface,
                action,
            }) => match action.as_str() {
                "toggle-caffeinate" => self.toggle_keep_awake(),
                other => {
                    if let Some(window_id) = other.strip_prefix("activate-window:")
                        && let Some(session_id) =
                            self.state.mux().selected_session().map(str::to_owned)
                    {
                        self.state.activate_window_from_ui(&session_id, window_id);
                    } else {
                        let _ = self.extensions.submit_ui_action(ExtensionUiAction {
                            module,
                            generation,
                            surface,
                            action: other.to_owned(),
                            payload: serde_json::Value::Null,
                        });
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
                generation,
                surface,
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
                    let _ = self.extensions.submit_ui_action(ExtensionUiAction {
                        module,
                        generation,
                        surface,
                        action: "reorder".to_owned(),
                        payload: serde_json::json!({ "source": source, "before": before }),
                    });
                }
            }
            None => {}
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
        let sidebar = chrome_config.sidebar;
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
            configured_sidebar_width
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
        for reorder in self.extensions.take_session_reorders() {
            self.state
                .reorder_session_before(&reorder.source, reorder.before.as_deref());
        }
        self.publish_extension_mux_view(sidebar);
        let (sidebar_module_items, sidebar_footer_items) = if sidebar {
            let mut body = Vec::new();
            let mut footer = Vec::new();
            for name in &self.state.config().sidebar.modules {
                for item in self
                    .extensions
                    .surface(SurfacePlacement::Sidebar, name)
                    .into_iter()
                    .flat_map(PublishedSurfaceSnapshot::into_items)
                {
                    if item.item.kind.as_deref() == Some("footer") {
                        footer.push(item);
                    } else {
                        body.push(item);
                    }
                }
            }
            let session_modules = &self.state.config().sidebar.session_modules;
            body = compose_session_module_items(
                body,
                session_modules.iter().flat_map(|name| {
                    self.extensions
                        .surface(SurfacePlacement::Session, name)
                        .into_iter()
                        .flat_map(PublishedSurfaceSnapshot::into_items)
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
            crate::ui::sidebar::build_sidebar_items_from_published_items(
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
        let status_left_padding = chrome::STATUS_EDGE_PAD;
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
        let terminal_cell_height = self.terminal_view.cell_height();
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
                        &mut self.chrome.sidebar_space_swipe,
                    ) && self.state.activate_space_from_ui(space_id)
                    {
                        ui.ctx().request_repaint();
                    }
                    let title_icon = title_visible.then(|| {
                        chrome::load_app_icon_texture(ui.ctx(), &mut self.chrome.app_icon_texture)
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
                            chrome::SidebarEvent::ExtensionAction(action) => {
                                if self.extensions.submit_ui_action(action).is_ok() {
                                    ui.ctx().request_repaint();
                                }
                            }
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
                    let max = (rect.width() - 120.0).max(120.0);
                    self.state.set_sidebar_width_live(raw.clamp(120.0, max));
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

        let pane_backing_color = notch_chrome_color.unwrap_or(palette.mantle);
        self.terminal_view.show(
            self.state,
            ui,
            terminal_rect,
            palette,
            pane_backing_color,
            notch_chrome_color,
        );
    }
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
