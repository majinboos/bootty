use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_render::{
    geometry::{
        CellMetrics, SurfacePoint, SurfaceRect, TerminalPadding, TerminalSurface, ViewTransform,
        fit_cell_height_to_available_space, fit_cell_width_to_available_space,
    },
    paint_plan::{CursorBlinkPhase, PaintPlanner, TerminalPaintPlan},
    terminal_render::{RenderFramePool, TerminalRenderCommand, TerminalRenderFrame},
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::{
        TerminalRendererId, terminal_render_callback_for_renderer, terminal_text_cell_metrics,
    },
};
use bootty_runtime::scheduler::CURSOR_BLINK_REFRESH_INTERVAL;
use bootty_terminal::terminal_frame::{CursorSnapshot, FrameScrollbar, RenderCell, RenderFrame};
use bootty_terminal::terminal_image::KittyImageFrame;
use eframe::{
    egui::{self, Color32, Pos2, Rect, Sense, Vec2},
    wgpu,
};

mod workspace_view;

pub(crate) use workspace_view::{TerminalWorkspaceView, animate_indeterminate_progress};

fn surface_rect(rect: Rect) -> SurfaceRect {
    SurfaceRect {
        min_x: rect.min.x,
        min_y: rect.min.y,
        max_x: rect.max.x,
        max_y: rect.max.y,
    }
}

fn surface_point(pos: Pos2) -> SurfacePoint {
    SurfacePoint { x: pos.x, y: pos.y }
}

#[derive(Default)]
pub struct TerminalWidget {
    renderer_id: TerminalRendererId,
    planner: PaintPlanner,
    metrics: RendererMetrics,
    cell: CellMetrics,
    base_cell: CellMetrics,
    text_config: TerminalTextConfig,
    cursor_blink: CursorBlinkClock,
    scrollbar: ScrollbarVisibility,
    target_format: Option<wgpu::TextureFormat>,
    render_cache: TerminalRenderCache,
    terminal_cursor_icon: egui::CursorIcon,
    search_pulse: SearchPulse,
    transition_key: Option<String>,
    transition_pending: bool,
    transition_source_frame: Option<Arc<RenderFrame>>,
    view: ViewTransform,
    last_surface: Option<SurfaceRect>,
}

pub use bootty_runtime::frame_source::TerminalFrameSource;

impl TerminalWidget {
    pub fn new(target_format: Option<wgpu::TextureFormat>) -> Self {
        Self {
            target_format,
            terminal_cursor_icon: egui::CursorIcon::Text,
            ..Self::default()
        }
    }

    pub fn with_text_config(mut self, text_config: TerminalTextConfig) -> Self {
        self.set_text_config(text_config);
        self
    }

    pub fn set_text_config(&mut self, text_config: TerminalTextConfig) {
        self.text_config = text_config;
        self.update_cell_metrics();
        self.render_cache.clear();
    }

    pub fn set_terminal_cursor_icon(&mut self, icon: egui::CursorIcon) {
        self.terminal_cursor_icon = icon;
    }

    // Drop the cached frame and transition state so an empty session stops painting the closed
    // terminal and the next tab starts from a clean slate.
    pub fn reset(&mut self) {
        self.render_cache.clear();
        self.transition_key = None;
        self.transition_pending = false;
        self.transition_source_frame = None;
    }

    pub fn is_zoomed(&self) -> bool {
        self.view.is_zoomed()
    }

    pub fn view_transform(&self) -> ViewTransform {
        self.view
    }

    pub fn apply_pinch(&mut self, factor: f32, focal: Option<Pos2>) {
        let Some(surface) = self.last_surface else {
            return;
        };
        let center = SurfacePoint {
            x: (surface.min_x + surface.max_x) * 0.5,
            y: (surface.min_y + surface.max_y) * 0.5,
        };
        let focal = focal.map(surface_point).unwrap_or(center);
        let focal = SurfacePoint {
            x: focal.x.clamp(surface.min_x, surface.max_x),
            y: focal.y.clamp(surface.min_y, surface.max_y),
        };
        self.view = self.view.pinched(factor, focal, surface);
    }

    pub fn apply_pan(&mut self, delta: Vec2) {
        let Some(surface) = self.last_surface else {
            return;
        };
        self.view = self.view.panned(delta.x, delta.y, surface);
    }

    pub fn set_transition_key(&mut self, key: Option<String>) {
        if self.transition_key == key {
            return;
        }
        self.transition_source_frame = self.render_cache.frame.clone();
        self.transition_key = key;
        self.transition_pending = true;
    }

    fn transition_frame_ready(&self, frame: &Arc<RenderFrame>) -> bool {
        !is_transition_placeholder_frame(frame)
            && !self
                .transition_source_frame
                .as_ref()
                .is_some_and(|source| Arc::ptr_eq(source, frame))
    }

    pub fn initial_geometry() -> bootty_render::geometry::TerminalGeometry {
        TerminalSurface::for_logical_size(
            1000.0,
            672.0,
            CellMetrics::default(),
            TerminalPadding::default(),
        )
        .geometry()
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        terminal: &mut dyn TerminalFrameSource,
    ) -> Result<TerminalWidgetOutput> {
        let available = ui.available_size_before_wrap();
        let desired = Vec2::new(available.x.max(320.0), available.y.max(240.0));
        let (rect, _) = ui.allocate_exact_size(desired, Sense::click_and_drag());
        self.show_at_rect(ui, rect, "terminal", terminal)
    }

    pub fn show_at_rect(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        terminal: &mut dyn TerminalFrameSource,
    ) -> Result<TerminalWidgetOutput> {
        let widget_id = ui.make_persistent_id(("terminal-widget", id_salt));
        let response = ui.interact(rect, widget_id, Sense::click_and_drag());
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }

        self.cell = self.cell_metrics_for_rect(rect);
        let surface = TerminalSurface::for_rect(surface_rect(rect), self.cell);
        terminal.set_display_scale(ui.ctx().pixels_per_point())?;
        terminal.set_render_cell_metrics(self.cell)?;
        terminal.resize(surface.geometry())?;

        let extract_start = Instant::now();
        let frame = terminal.extract_frame()?;
        self.metrics.extract_total_us = extract_start.elapsed().as_micros() as u64;
        // Match the grid rect the renderer projects through, so pinch/pan math agrees with it.
        self.last_surface = Some(surface.grid_rect(frame.cols, frame.rows));
        let viewport_scroll_delta =
            self.handle_scrollbar_interaction(ui, widget_id, surface, frame.as_ref());
        self.paint(ui, surface, &frame)?;
        self.handle_hyperlink_interaction(ui, surface, frame.as_ref(), &response);
        self.metrics.render_state_update_us = frame.stats.render_state_update_us;
        self.metrics.frame_extraction_us = frame.stats.extraction_us;
        self.metrics.cells = frame.stats.cells;
        self.metrics.chars = frame.stats.chars;
        self.metrics.dirty_rows = frame.stats.dirty_rows;
        self.metrics.image_placements = frame.images.placements.len();
        self.metrics.virtual_placements = frame.images.virtual_placements.len();
        Ok(TerminalWidgetOutput {
            surface,
            viewport_scroll_delta,
        })
    }

    pub fn metrics(&self) -> RendererMetrics {
        self.metrics
    }

    pub fn cell_size(&self) -> (u32, u32) {
        self.cell.rounded_size()
    }
    pub fn cell_dimensions(&self) -> (f32, f32) {
        (self.cell.width, self.cell.height)
    }

    pub fn geometry_for_rect(&self, rect: Rect) -> bootty_render::geometry::TerminalGeometry {
        TerminalSurface::for_rect(surface_rect(rect), self.cell_metrics_for_rect(rect)).geometry()
    }
    fn cell_metrics_for_rect(&self, rect: Rect) -> CellMetrics {
        let mut cell = self.base_cell;
        if self.text_config.fit_cell_height {
            cell = fit_cell_height_to_available_space(rect.height(), cell, Default::default());
        }
        if self.text_config.fit_cell_width {
            cell = fit_cell_width_to_available_space(rect.width(), cell, Default::default());
        }
        cell
    }

    fn handle_hyperlink_interaction(
        &self,
        ui: &mut egui::Ui,
        surface: TerminalSurface,
        frame: &RenderFrame,
        response: &egui::Response,
    ) {
        let hovered_link = response
            .hovered()
            .then(|| ui.input(|input| input.pointer.hover_pos()))
            .flatten()
            .and_then(|pos| {
                hyperlink_at(frame, surface, self.view.inverse_point(surface_point(pos)))
            });

        if let Some(link) = hovered_link {
            let modifiers = ui.input(|input| input.modifiers);
            ui.ctx().set_cursor_icon(if modifiers.command {
                egui::CursorIcon::PointingHand
            } else {
                self.terminal_cursor_icon
            });
            let rect = transformed_surface_rect(
                surface.run_rect(link.start_col, link.row, link.cells),
                self.view,
            );
            ui.painter().hline(
                rect.x_range(),
                rect.max.y - 1.0,
                egui::Stroke::new(1.0, ui.visuals().hyperlink_color),
            );
            if response.clicked() && modifiers.command {
                ui.ctx().open_url(egui::OpenUrl::new_tab(link.url));
            }
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(self.terminal_cursor_icon);
        }
    }

    fn update_cell_metrics(&mut self) {
        self.base_cell = terminal_text_cell_metrics(&self.text_config);
        self.cell = self.base_cell;
    }
    fn paint(
        &mut self,
        ui: &mut egui::Ui,
        surface: TerminalSurface,
        frame: &Arc<RenderFrame>,
    ) -> Result<()> {
        let paint_start = Instant::now();
        anyhow::ensure!(
            self.target_format.is_some(),
            "terminal renderer requires an eframe WGPU target format"
        );
        let transition_ready = self.transition_frame_ready(frame);
        let frame = self
            .render_cache
            .frame_for_paint(frame, self.transition_pending && !transition_ready);
        if transition_ready {
            self.transition_pending = false;
            self.transition_source_frame = None;
        }
        let cursor_blinking = frame.cursor.is_some_and(|cursor| cursor.blinking);
        let cursor_blink_phase = self.cursor_blink.phase(Instant::now(), frame.cursor);
        if !self.render_cache.matches(surface, &frame) {
            let plan = self
                .planner
                .plan_with_cursor_blink_phase_and_text_cell_height(
                    surface,
                    &frame,
                    self.text_config.font_size,
                    self.base_cell.height,
                    CursorBlinkPhase::visible(),
                );
            let text_contract =
                TerminalTextContract::for_terminal_paint_plan(plan, &self.text_config);
            let text_runs = plan.text_runs.len();
            self.render_cache.rebuild(
                surface,
                &frame,
                plan,
                &text_contract,
                &frame.images,
                text_runs,
            );
        }
        self.render_cache.apply_cursor_phase(cursor_blink_phase);
        paint_terminal_content(
            ui,
            self.renderer_id.clone(),
            self.render_cache.render_frame(),
            self.target_format,
            self.view,
        );
        self.paint_search_pulse(ui, surface, frame.as_ref());
        self.paint_copy_mode_position_overlay(ui, surface, frame.as_ref());
        self.metrics.cursor_blinking = cursor_blinking;
        self.metrics.text_runs = self.render_cache.text_runs();
        self.paint_scrollbar(ui, surface, frame.as_ref());
        // A blink frame repaints the whole window, so only animate while the window is focused:
        // an unfocused window shows a static cursor instead of costing a frame every 33 ms.
        // Regaining focus is itself a repaint, which restarts the animation.
        if cursor_blinking && window_focused(ui.ctx()) {
            ui.ctx()
                .request_repaint_after(CURSOR_BLINK_REFRESH_INTERVAL);
        }
        self.metrics.paint_us = paint_start.elapsed().as_micros() as u64;
        Ok(())
    }

    fn paint_scrollbar(
        &mut self,
        ui: &mut egui::Ui,
        surface: TerminalSurface,
        frame: &RenderFrame,
    ) {
        let Some(scrollbar) = frame.scrollbar else {
            return;
        };
        if !is_scrollbar_scrollable(scrollbar) {
            self.scrollbar.last_offset = Some(scrollbar.offset);
            return;
        }

        let active = self.scrollbar.update_activity(scrollbar, Instant::now());
        if !active && !self.scrollbar.dragging {
            return;
        }
        ui.ctx()
            .request_repaint_after(SCROLLBAR_VISIBLE_AFTER_SCROLL);

        paint_scrollbar(ui, surface, frame, scrollbar, self.scrollbar.thumb_hovered);
    }

    fn handle_scrollbar_interaction(
        &mut self,
        ui: &mut egui::Ui,
        widget_id: egui::Id,
        surface: TerminalSurface,
        frame: &RenderFrame,
    ) -> isize {
        let Some(scrollbar) = frame.scrollbar else {
            self.scrollbar.thumb_hovered = false;
            return 0;
        };
        if !is_scrollbar_scrollable(scrollbar) {
            self.scrollbar.thumb_hovered = false;
            return 0;
        }

        let now = Instant::now();
        self.scrollbar.update_activity(scrollbar, now);

        let area_response = ui.interact(
            scrollbar_hit_rect(surface),
            widget_id.with("terminal-scrollbar-area"),
            Sense::hover(),
        );
        if area_response.hovered() {
            self.scrollbar.active_until = Some(now + SCROLLBAR_VISIBLE_AFTER_SCROLL);
        }

        let active = self
            .scrollbar
            .active_until
            .is_some_and(|until| now <= until);
        if !active && !self.scrollbar.dragging {
            self.scrollbar.thumb_hovered = false;
            return 0;
        }

        let thumb = scrollbar_thumb_rect(surface, scrollbar, false);
        let response = ui.interact(
            thumb.expand(6.0),
            widget_id.with("terminal-scrollbar-thumb"),
            Sense::click_and_drag(),
        );
        self.scrollbar.thumb_hovered = response.hovered();
        if response.drag_started() {
            self.scrollbar.dragging = true;
            self.scrollbar.drag_last_y = response.interact_pointer_pos().map(|pos| pos.y);
            self.scrollbar.active_until = Some(Instant::now() + SCROLLBAR_VISIBLE_AFTER_SCROLL);
        }
        if response.drag_stopped() {
            self.scrollbar.dragging = false;
            self.scrollbar.drag_last_y = None;
        }
        let mut viewport_scroll_delta = 0;
        if response.dragged()
            && let (Some(last_y), Some(pos)) =
                (self.scrollbar.drag_last_y, response.interact_pointer_pos())
        {
            let delta = scrollbar_drag_delta_rows(surface, scrollbar, pos.y - last_y);
            if delta != 0 {
                viewport_scroll_delta = delta;
                self.scrollbar.drag_last_y = Some(pos.y);
                self.scrollbar.active_until = Some(Instant::now() + SCROLLBAR_VISIBLE_AFTER_SCROLL);
            }
        }
        viewport_scroll_delta
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalWidgetOutput {
    pub surface: TerminalSurface,
    pub viewport_scroll_delta: isize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererMetrics {
    pub extract_total_us: u64,
    pub render_state_update_us: u64,
    pub frame_extraction_us: u64,
    pub paint_us: u64,
    pub cells: usize,
    pub chars: usize,
    pub dirty_rows: usize,
    pub image_placements: usize,
    pub virtual_placements: usize,
    pub text_runs: usize,
    pub cursor_blinking: bool,
}

const CURSOR_BLINK_PERIOD: Duration = Duration::from_millis(1_400);
const SCROLLBAR_VISIBLE_AFTER_SCROLL: Duration = Duration::from_millis(900);
const SCROLLBAR_HIT_WIDTH: f32 = 16.0;

struct TerminalRenderCache {
    frame: Option<Arc<RenderFrame>>,
    surface: Option<TerminalSurface>,
    render_frame: TerminalRenderFrame,
    pool: RenderFramePool,
    visible_cursor_tail: Vec<TerminalRenderCommand>,
    cursor_tail_start: Option<usize>,
    cursor_alpha: Option<u8>,
    text_runs: usize,
}

impl Default for TerminalRenderCache {
    fn default() -> Self {
        Self {
            frame: None,
            surface: None,
            render_frame: TerminalRenderFrame {
                surface: SurfaceRect::from_min_size(0.0, 0.0, 0.0, 0.0),
                commands: Vec::new(),
            },
            pool: RenderFramePool::default(),
            visible_cursor_tail: Vec::new(),
            cursor_tail_start: None,
            cursor_alpha: None,
            text_runs: 0,
        }
    }
}

impl TerminalRenderCache {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn matches(&self, surface: TerminalSurface, frame: &Arc<RenderFrame>) -> bool {
        self.surface == Some(surface)
            && self
                .frame
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(cached, frame))
    }

    fn frame_for_paint(
        &self,
        incoming: &Arc<RenderFrame>,
        hold_transition_placeholder: bool,
    ) -> Arc<RenderFrame> {
        if hold_transition_placeholder && is_transition_placeholder_frame(incoming) {
            self.frame
                .as_ref()
                .filter(|cached| {
                    !is_transition_placeholder_frame(cached) && cached.images.placements.is_empty()
                })
                .map(Arc::clone)
                .unwrap_or_else(|| Arc::clone(incoming))
        } else {
            Arc::clone(incoming)
        }
    }

    /// Rebuild the cached render frame in place from `plan`, recycling the previous
    /// frame's command buffer and text strings via the pool, then refresh the cache
    /// bookkeeping. This is the hot repaint path; it avoids the ~500 allocations a
    /// fresh `from_plan_and_images` churns per changed frame.
    fn rebuild(
        &mut self,
        surface: TerminalSurface,
        frame: &Arc<RenderFrame>,
        plan: &TerminalPaintPlan,
        text_contract: &TerminalTextContract,
        images: &KittyImageFrame,
        text_runs: usize,
    ) {
        self.pool
            .rebuild_from_plan_and_images(&mut self.render_frame, plan, text_contract, images);
        self.finish_store(surface, frame, text_runs);
    }

    fn finish_store(
        &mut self,
        surface: TerminalSurface,
        frame: &Arc<RenderFrame>,
        text_runs: usize,
    ) {
        self.cursor_tail_start = self
            .render_frame
            .commands
            .iter()
            .position(|command| matches!(command, TerminalRenderCommand::Cursor(_)));
        self.visible_cursor_tail.clear();
        if let Some(start) = self.cursor_tail_start {
            self.visible_cursor_tail
                .extend_from_slice(&self.render_frame.commands[start..]);
        }
        self.frame = Some(Arc::clone(frame));
        self.surface = Some(surface);
        self.cursor_alpha = None;
        self.text_runs = text_runs;
    }

    fn apply_cursor_phase(&mut self, phase: CursorBlinkPhase) {
        let Some(start) = self.cursor_tail_start else {
            return;
        };
        let alpha = cursor_blink_alpha(phase);
        if self.cursor_alpha == Some(alpha) {
            return;
        }

        self.render_frame.commands.truncate(start);
        if alpha > 0 {
            self.render_frame.commands.extend(
                self.visible_cursor_tail
                    .iter()
                    .cloned()
                    .map(|command| cursor_tail_command_with_alpha(command, alpha)),
            );
        }
        self.cursor_alpha = Some(alpha);
    }

    fn render_frame(&self) -> &TerminalRenderFrame {
        &self.render_frame
    }

    fn text_runs(&self) -> usize {
        self.text_runs
    }
}

fn is_transition_placeholder_frame(frame: &RenderFrame) -> bool {
    frame.cols == 0
        || frame.rows == 0
        || (frame.cells.is_empty()
            && frame.text.is_empty()
            && frame.images.placements.is_empty()
            && frame.images.virtual_placements.is_empty()
            && frame.images.virtual_placeholder_rows.is_empty())
}

fn cursor_tail_command_with_alpha(
    mut command: TerminalRenderCommand,
    alpha: u8,
) -> TerminalRenderCommand {
    match &mut command {
        TerminalRenderCommand::Cursor(cursor) => cursor.color.a = alpha,
        TerminalRenderCommand::Text(text) => text.attrs.fg.a = alpha,
        TerminalRenderCommand::Sprite(sprite) => sprite.color.a = alpha,
        TerminalRenderCommand::FillRect(_)
        | TerminalRenderCommand::Image(_)
        | TerminalRenderCommand::KittyVirtualPlacement(_)
        | TerminalRenderCommand::Decoration(_) => {}
    }
    command
}

fn cursor_blink_alpha(phase: CursorBlinkPhase) -> u8 {
    (phase.opacity() * 255.0).round().clamp(0.0, 255.0) as u8
}

#[derive(Default)]
struct SearchPulse {
    last_pulse: u64,
    started_at: Option<Instant>,
}

impl TerminalWidget {
    fn paint_search_pulse(
        &mut self,
        ui: &mut egui::Ui,
        surface: TerminalSurface,
        frame: &RenderFrame,
    ) {
        let Some(selection) = frame.active_search_match else {
            self.search_pulse.started_at = None;
            self.search_pulse.last_pulse = frame.search_pulse;
            return;
        };
        if frame.search_pulse == 0 {
            self.search_pulse.last_pulse = 0;
            self.search_pulse.started_at = None;
            return;
        }
        let now = Instant::now();
        if self.search_pulse.last_pulse != frame.search_pulse {
            self.search_pulse.last_pulse = frame.search_pulse;
            self.search_pulse.started_at = Some(now);
        }
        let Some(started_at) = self.search_pulse.started_at else {
            return;
        };
        let elapsed = now.duration_since(started_at);
        if elapsed >= SEARCH_PULSE_DURATION {
            self.search_pulse.started_at = None;
            return;
        }

        let cells = selection
            .end_col
            .saturating_sub(selection.start_col)
            .saturating_add(1);
        let rect = surface.run_rect(selection.start_col, selection.row, cells);
        let rect = transformed_surface_rect(rect, self.view).expand(2.0 + 7.0 * pulse_t(elapsed));
        let alpha = ((1.0 - pulse_t(elapsed)) * 180.0).round() as u8;
        ui.painter().rect_stroke(
            rect,
            5.0,
            egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 238, 128, alpha)),
            egui::StrokeKind::Outside,
        );
        ui.ctx().request_repaint();
    }

    fn paint_copy_mode_position_overlay(
        &self,
        ui: &mut egui::Ui,
        surface: TerminalSurface,
        frame: &RenderFrame,
    ) {
        let Some(label) = copy_mode_position_label(frame) else {
            return;
        };

        let foreground = frame.colors.foreground;
        let background = frame.colors.background;
        let text_color = Color32::from_rgb(foreground.r, foreground.g, foreground.b);
        let fill = Color32::from_rgba_unmultiplied(background.r, background.g, background.b, 220);
        let stroke = Color32::from_rgba_unmultiplied(foreground.r, foreground.g, foreground.b, 100);
        let font_id =
            egui::FontId::monospace((self.text_config.font_size * 0.78).clamp(10.0, 14.0));
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(label, font_id, text_color);
        let padding = egui::vec2(6.0, 3.0);
        let grid = transformed_surface_rect(surface.grid_rect(frame.cols, frame.rows), self.view);
        let scrollbar_clearance = frame.scrollbar.map_or(0.0, |_| SCROLLBAR_HIT_WIDTH);
        let rect = Rect::from_min_size(
            Pos2::new(
                grid.right() - scrollbar_clearance - galley.size().x - padding.x * 2.0 - 6.0,
                grid.top() + 6.0,
            ),
            galley.size() + padding * 2.0,
        );
        painter.rect_filled(rect, 4.0, fill);
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Outside,
        );
        painter.galley(rect.min + padding, galley, text_color);
    }
}

fn copy_mode_position_label(frame: &RenderFrame) -> Option<String> {
    frame.copy_mode?;
    let cursor = frame.cursor?;
    let total = frame
        .scrollbar
        .map_or(u64::from(frame.rows.max(1)), |scrollbar| {
            scrollbar.total.max(1)
        });
    let offset = frame.scrollbar.map_or(0, |scrollbar| scrollbar.offset);
    let current = offset
        .saturating_add(u64::from(cursor.y))
        .saturating_add(1)
        .min(total);
    Some(format!("[{current}/{total}]"))
}

const SEARCH_PULSE_DURATION: Duration = Duration::from_millis(180);

fn pulse_t(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / SEARCH_PULSE_DURATION.as_secs_f32()).clamp(0.0, 1.0)
}

fn transformed_surface_rect(rect: SurfaceRect, view: ViewTransform) -> Rect {
    Rect::from_min_max(
        Pos2::new(
            rect.min_x * view.zoom + view.pan_x,
            rect.min_y * view.zoom + view.pan_y,
        ),
        Pos2::new(
            rect.max_x * view.zoom + view.pan_x,
            rect.max_y * view.zoom + view.pan_y,
        ),
    )
}

#[derive(Default)]
struct ScrollbarVisibility {
    last_offset: Option<u64>,
    active_until: Option<Instant>,
    dragging: bool,
    drag_last_y: Option<f32>,
    thumb_hovered: bool,
}
impl ScrollbarVisibility {
    fn update_activity(&mut self, scrollbar: FrameScrollbar, now: Instant) -> bool {
        if self
            .last_offset
            .is_some_and(|offset| offset != scrollbar.offset)
        {
            self.active_until = Some(now + SCROLLBAR_VISIBLE_AFTER_SCROLL);
        }
        self.last_offset = Some(scrollbar.offset);
        self.active_until.is_some_and(|until| now <= until)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HoveredLink {
    url: String,
    row: u16,
    start_col: u16,
    cells: u16,
}

fn hyperlink_at(
    frame: &RenderFrame,
    surface: TerminalSurface,
    pos: SurfacePoint,
) -> Option<HoveredLink> {
    if !surface.rect.contains(pos) {
        return None;
    }
    let point = surface.surface_to_grid(pos);
    if point.x >= frame.cols || point.y >= frame.rows {
        return None;
    }
    let row = frame
        .cells
        .iter()
        .filter(|cell| cell.y == point.y)
        .collect::<Vec<_>>();
    let hovered = row.iter().position(|cell| cell.x == point.x)?;
    osc8_link_at(&row, hovered).or_else(|| plain_url_at(frame, &row, hovered))
}

fn osc8_link_at(row: &[&RenderCell], hovered: usize) -> Option<HoveredLink> {
    let url = row[hovered].hyperlink.clone()?;
    let same_link = |cell: &&RenderCell| cell.hyperlink.as_deref() == Some(url.as_str());
    let start = (0..=hovered)
        .rev()
        .take_while(|index| same_link(&row[*index]))
        .last()
        .unwrap_or(hovered);
    let end = (hovered..row.len())
        .take_while(|index| same_link(&row[*index]))
        .last()
        .unwrap_or(hovered);
    Some(link_over_run(url, row, start, end))
}

fn plain_url_at(frame: &RenderFrame, row: &[&RenderCell], hovered: usize) -> Option<HoveredLink> {
    let is_token = |cell: &&RenderCell| frame.cell_text(cell).iter().all(|ch| !ch.is_whitespace());
    let mut start = (0..=hovered)
        .rev()
        .take_while(|index| is_token(&row[*index]))
        .last()
        .unwrap_or(hovered);
    let mut end = (hovered..row.len())
        .take_while(|index| is_token(&row[*index]))
        .last()
        .unwrap_or(hovered);
    let is_edge_punctuation = |cell: &&RenderCell| {
        frame.cell_text(cell).iter().all(|ch| {
            matches!(
                ch,
                '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
            )
        })
    };
    while start < end && is_edge_punctuation(&row[start]) {
        start += 1;
    }
    while end > start && is_edge_punctuation(&row[end]) {
        end -= 1;
    }
    let url = row[start..=end]
        .iter()
        .flat_map(|cell| frame.cell_text(cell))
        .collect::<String>();
    (url.starts_with("http://") || url.starts_with("https://"))
        .then(|| link_over_run(url, row, start, end))
}

fn link_over_run(url: String, row: &[&RenderCell], start: usize, end: usize) -> HoveredLink {
    HoveredLink {
        url,
        row: row[start].y,
        start_col: row[start].x,
        cells: row[end].x - row[start].x + 1,
    }
}

#[derive(Default)]
struct CursorBlinkClock {
    started_at: Option<Instant>,
    cursor: Option<CursorBlinkKey>,
}

impl CursorBlinkClock {
    fn phase(&mut self, now: Instant, cursor: Option<CursorSnapshot>) -> CursorBlinkPhase {
        let Some(cursor) = cursor else {
            self.started_at = None;
            self.cursor = None;
            return CursorBlinkPhase::visible();
        };
        if !cursor.blinking {
            self.started_at = None;
            self.cursor = Some(CursorBlinkKey::from(cursor));
            return CursorBlinkPhase::visible();
        }

        let cursor_key = CursorBlinkKey::from(cursor);
        if self.cursor != Some(cursor_key) {
            self.started_at = Some(now);
            self.cursor = Some(cursor_key);
            return CursorBlinkPhase::visible();
        }

        let started_at = *self.started_at.get_or_insert(now);
        CursorBlinkPhase::from_opacity(cursor_blink_opacity(now.duration_since(started_at)))
    }
}

fn paint_scrollbar(
    ui: &mut egui::Ui,
    surface: TerminalSurface,
    frame: &RenderFrame,
    scrollbar: FrameScrollbar,
    hovered: bool,
) {
    let thumb = scrollbar_thumb_rect(surface, scrollbar, hovered);
    let color = frame.colors.foreground;
    ui.painter().rect_filled(
        thumb,
        2.0,
        Color32::from_rgba_unmultiplied(color.r, color.g, color.b, 120),
    );
}

pub(crate) fn scrollbar_hit_rect(surface: TerminalSurface) -> Rect {
    let track = surface.rect;
    Rect::from_min_max(
        Pos2::new(track.max_x - SCROLLBAR_HIT_WIDTH, track.min_y),
        Pos2::new(track.max_x, track.max_y),
    )
}

fn is_scrollbar_scrollable(scrollbar: FrameScrollbar) -> bool {
    scrollbar.total > scrollbar.len && scrollbar.len > 0
}

fn scrollbar_thumb_rect(
    surface: TerminalSurface,
    scrollbar: FrameScrollbar,
    hovered: bool,
) -> Rect {
    let track = surface.rect;
    let total = scrollbar.total.max(1) as f32;
    let len = scrollbar.len.min(scrollbar.total).max(1) as f32;
    let offset = scrollbar
        .offset
        .min(scrollbar.total.saturating_sub(scrollbar.len)) as f32;
    let scale = if hovered { 1.2 } else { 1.0 };
    let base_width = 4.0;
    let thumb_width = base_width * scale;
    let thumb_height = (track.height() * (len / total)).clamp(28.0, track.height());
    let travel = (track.height() - thumb_height).max(0.0);
    let max_offset = scrollbar.total.saturating_sub(scrollbar.len).max(1) as f32;
    let thumb_top = track.min_y + travel * (offset / max_offset);
    Rect::from_min_size(
        Pos2::new(track.max_x - thumb_width - 3.0, thumb_top),
        Vec2::new(thumb_width, thumb_height),
    )
}

fn scrollbar_drag_delta_rows(
    surface: TerminalSurface,
    scrollbar: FrameScrollbar,
    delta_y: f32,
) -> isize {
    let thumb = scrollbar_thumb_rect(surface, scrollbar, false);
    let travel = (surface.rect.height() - thumb.height()).max(1.0);
    let max_offset = scrollbar.total.saturating_sub(scrollbar.len).max(1) as f32;
    (delta_y / travel * max_offset).round() as isize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorBlinkKey {
    x: u16,
    y: u16,
    at_wide_tail: bool,
}

impl From<CursorSnapshot> for CursorBlinkKey {
    fn from(cursor: CursorSnapshot) -> Self {
        Self {
            x: cursor.x,
            y: cursor.y,
            at_wide_tail: cursor.at_wide_tail,
        }
    }
}

/// Whether the window has keyboard focus. Unknown counts as focused, so a platform that never
/// reports focus keeps animating rather than freezing the cursor.
fn window_focused(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.viewport().focused.unwrap_or(true))
}

fn cursor_blink_opacity(elapsed: Duration) -> f32 {
    let period = CURSOR_BLINK_PERIOD.as_secs_f32();
    let phase = (elapsed.as_secs_f32() % period) / period;
    (0.5 + 0.5 * (phase * std::f32::consts::TAU).cos()).clamp(0.0, 1.0)
}

fn paint_terminal_content(
    ui: &mut egui::Ui,
    renderer_id: TerminalRendererId,
    frame: &TerminalRenderFrame,
    target_format: Option<wgpu::TextureFormat>,
    view: ViewTransform,
) {
    let Some(callback) = target_format.and_then(|target_format| {
        terminal_render_callback_for_renderer(renderer_id, frame, target_format, view)
    }) else {
        return;
    };
    let rect = frame.surface;
    ui.painter_at(Rect::from_min_max(
        Pos2::new(rect.min_x, rect.min_y),
        Pos2::new(rect.max_x, rect.max_y),
    ))
    .add(callback);
}
