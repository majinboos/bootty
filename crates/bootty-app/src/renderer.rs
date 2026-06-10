use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::{
    egui::{self, Color32, Pos2, Rect, Sense, Vec2},
    wgpu,
};

use crate::{
    geometry::{CellMetrics, SurfaceRect, TerminalSurface},
    paint_plan::{CursorBlinkPhase, PaintPlanner},
    scheduler::CURSOR_BLINK_REFRESH_INTERVAL,
    terminal::{CursorSnapshot, RenderFrame},
    terminal_render::{TerminalRenderCommand, TerminalRenderFrame},
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::{terminal_render_callback, terminal_text_cell_metrics},
};

#[derive(Default)]
pub struct TerminalWidget {
    planner: PaintPlanner,
    metrics: RendererMetrics,
    cell: CellMetrics,
    text_config: TerminalTextConfig,
    cursor_blink: CursorBlinkClock,
    scrollbar: ScrollbarVisibility,
    target_format: Option<wgpu::TextureFormat>,
    render_cache: TerminalRenderCache,
}

pub use bootty_runtime::render_source::TerminalRenderSource;

impl TerminalWidget {
    pub fn new(target_format: Option<wgpu::TextureFormat>) -> Self {
        Self {
            target_format,
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

    pub fn initial_geometry() -> crate::geometry::TerminalGeometry {
        TerminalSurface::default_for_size(Vec2::new(1000.0, 672.0)).geometry()
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        terminal: &mut dyn TerminalRenderSource,
    ) -> Result<TerminalSurface> {
        let available = ui.available_size_before_wrap();
        let desired = Vec2::new(available.x.max(320.0), available.y.max(240.0));
        let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
        if response.clicked() {
            response.request_focus();
        }

        let surface = TerminalSurface::for_rect(rect, self.cell);
        terminal.resize(surface.geometry())?;

        let extract_start = Instant::now();
        let frame = terminal.extract_frame()?;
        self.metrics.extract_total_us = extract_start.elapsed().as_micros() as u64;
        self.handle_scrollbar_interaction(ui, surface, frame.as_ref(), terminal)?;
        self.paint(ui, surface, &frame)?;
        self.metrics.render_state_update_us = frame.stats.render_state_update_us;
        self.metrics.frame_extraction_us = frame.stats.extraction_us;
        self.metrics.cells = frame.stats.cells;
        self.metrics.chars = frame.stats.chars;
        self.metrics.dirty_rows = frame.stats.dirty_rows;
        self.metrics.image_placements = frame.images.placements.len();
        self.metrics.virtual_placements = frame.images.virtual_placements.len();
        Ok(surface)
    }

    pub fn metrics(&self) -> RendererMetrics {
        self.metrics
    }

    pub fn cell_size(&self) -> (u32, u32) {
        self.cell.rounded_size()
    }

    fn update_cell_metrics(&mut self) {
        self.cell = terminal_text_cell_metrics(&self.text_config);
    }
    fn paint(
        &mut self,
        ui: &mut egui::Ui,
        surface: TerminalSurface,
        frame: &Arc<crate::terminal::RenderFrame>,
    ) -> Result<()> {
        let paint_start = Instant::now();
        anyhow::ensure!(
            self.target_format.is_some(),
            "terminal renderer requires an eframe WGPU target format"
        );
        let cursor_blinking = frame.cursor.is_some_and(|cursor| cursor.blinking);
        let cursor_blink_phase = self.cursor_blink.phase(Instant::now(), frame.cursor);
        if !self.render_cache.matches(surface, frame) {
            let plan = self.planner.plan_with_cursor_blink_phase(
                surface,
                frame,
                self.text_config.font_size,
                CursorBlinkPhase::visible(),
            );
            let text_contract =
                TerminalTextContract::for_terminal_paint_plan(plan, &self.text_config);
            let text_runs = plan.text_runs.len();
            let render_frame =
                TerminalRenderFrame::from_plan_and_images(plan, &text_contract, &frame.images);
            self.render_cache
                .store(surface, frame, render_frame, text_runs);
        }
        self.render_cache.apply_cursor_phase(cursor_blink_phase);
        paint_terminal_content(ui, self.render_cache.render_frame(), self.target_format);
        self.metrics.cursor_blinking = cursor_blinking;
        self.metrics.text_runs = self.render_cache.text_runs();
        self.paint_scrollbar(ui, surface, frame.as_ref());
        if cursor_blinking {
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
        frame: &crate::terminal::RenderFrame,
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
        surface: TerminalSurface,
        frame: &crate::terminal::RenderFrame,
        terminal: &mut dyn TerminalRenderSource,
    ) -> Result<()> {
        let Some(scrollbar) = frame.scrollbar else {
            self.scrollbar.thumb_hovered = false;
            return Ok(());
        };
        if !is_scrollbar_scrollable(scrollbar) {
            self.scrollbar.thumb_hovered = false;
            return Ok(());
        }

        let now = Instant::now();
        self.scrollbar.update_activity(scrollbar, now);

        let area_response = ui.interact(
            scrollbar_hit_rect(surface),
            ui.make_persistent_id("terminal-scrollbar-area"),
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
            return Ok(());
        }

        let thumb = scrollbar_thumb_rect(surface, scrollbar, false);
        let response = ui.interact(
            thumb.expand(6.0),
            ui.make_persistent_id("terminal-scrollbar-thumb"),
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
        if response.dragged()
            && let (Some(last_y), Some(pos)) =
                (self.scrollbar.drag_last_y, response.interact_pointer_pos())
        {
            let delta = scrollbar_drag_delta_rows(surface, scrollbar, pos.y - last_y);
            if delta != 0 {
                terminal.scroll_viewport_delta(delta)?;
                self.scrollbar.drag_last_y = Some(pos.y);
                self.scrollbar.active_until = Some(Instant::now() + SCROLLBAR_VISIBLE_AFTER_SCROLL);
            }
        }
        Ok(())
    }
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

    fn store(
        &mut self,
        surface: TerminalSurface,
        frame: &Arc<RenderFrame>,
        render_frame: TerminalRenderFrame,
        text_runs: usize,
    ) {
        self.cursor_tail_start = render_frame
            .commands
            .iter()
            .position(|command| matches!(command, TerminalRenderCommand::Cursor(_)));
        self.visible_cursor_tail = self
            .cursor_tail_start
            .map(|start| render_frame.commands[start..].to_vec())
            .unwrap_or_default();
        self.frame = Some(Arc::clone(frame));
        self.surface = Some(surface);
        self.render_frame = render_frame;
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
struct ScrollbarVisibility {
    last_offset: Option<u64>,
    active_until: Option<Instant>,
    dragging: bool,
    drag_last_y: Option<f32>,
    thumb_hovered: bool,
}
impl ScrollbarVisibility {
    fn update_activity(
        &mut self,
        scrollbar: crate::terminal::FrameScrollbar,
        now: Instant,
    ) -> bool {
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
    frame: &crate::terminal::RenderFrame,
    scrollbar: crate::terminal::FrameScrollbar,
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
        Pos2::new(track.right() - SCROLLBAR_HIT_WIDTH, track.top()),
        Pos2::new(track.right(), track.bottom()),
    )
}

fn is_scrollbar_scrollable(scrollbar: crate::terminal::FrameScrollbar) -> bool {
    scrollbar.total > scrollbar.len && scrollbar.len > 0
}

fn scrollbar_thumb_rect(
    surface: TerminalSurface,
    scrollbar: crate::terminal::FrameScrollbar,
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
    let thumb_top = track.top() + travel * (offset / max_offset);
    Rect::from_min_size(
        Pos2::new(track.right() - thumb_width - 3.0, thumb_top),
        Vec2::new(thumb_width, thumb_height),
    )
}

fn scrollbar_drag_delta_rows(
    surface: TerminalSurface,
    scrollbar: crate::terminal::FrameScrollbar,
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

fn cursor_blink_opacity(elapsed: Duration) -> f32 {
    let period = CURSOR_BLINK_PERIOD.as_secs_f32();
    let phase = (elapsed.as_secs_f32() % period) / period;
    (0.5 + 0.5 * (phase * std::f32::consts::TAU).cos()).clamp(0.0, 1.0)
}

fn paint_terminal_content(
    ui: &mut egui::Ui,
    frame: &TerminalRenderFrame,
    target_format: Option<wgpu::TextureFormat>,
) {
    let Some(callback) = terminal_render_shape(frame, target_format) else {
        return;
    };
    ui.painter_at(egui_rect(frame.surface)).add(callback);
}

fn terminal_render_shape(
    frame: &TerminalRenderFrame,
    target_format: Option<wgpu::TextureFormat>,
) -> Option<egui::Shape> {
    let target_format = target_format?;
    terminal_render_callback(frame, target_format)
}

fn egui_rect(rect: SurfaceRect) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect.min_x, rect.min_y),
        Pos2::new(rect.max_x, rect.max_y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::{CellMetrics, DEFAULT_FONT_SIZE, TerminalGeometry, TerminalPadding},
        paint_plan::{CursorShape, PlanColor, TerminalPaintPlan},
        terminal::{CursorSnapshot, FrameColors, RenderFrame, TerminalEngine},
        terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement},
        terminal_render::{CursorCommand, FillCommand, FillRole, TerminalRenderCommand},
        terminal_text::terminal_text_config_for_plan,
    };
    use libghostty_vt::{
        render::{CursorVisualStyle, Dirty},
        style::RgbColor,
    };
    use std::sync::Arc;

    fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
        RgbColor { r, g, b }
    }

    fn cursor_at(x: u16, y: u16, blinking: bool) -> CursorSnapshot {
        CursorSnapshot {
            x,
            y,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking,
            color: None,
        }
    }

    #[test]
    fn widget_planning_feeds_terminal_background_render_commands_without_window() {
        let mut widget = TerminalWidget::default();
        let surface = TerminalSurface::for_size(
            Vec2::new(80.0, 40.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );
        let frame = RenderFrame {
            cols: 8,
            rows: 2,
            dirty: Dirty::Full,
            colors: FrameColors {
                background: rgb(12, 34, 56),
                foreground: rgb(220, 221, 222),
                cursor: None,
                ..Default::default()
            },
            cursor: None,
            row_dirty: vec![true, true],
            cells: Vec::new(),
            text: Vec::new(),
            images: Default::default(),
            scrollbar: None,
            stats: Default::default(),
        };

        let plan = widget.planner.plan(surface, &frame, DEFAULT_FONT_SIZE);
        let render_frame = TerminalRenderFrame::background_from_plan(plan);

        assert!(matches!(
            render_frame.commands.first(),
            Some(TerminalRenderCommand::FillRect(fill))
                if fill.role == FillRole::SurfaceBackground
                    && fill.color == PlanColor { r: 12, g: 34, b: 56, a: 255 }
        ));
    }

    #[test]
    fn widget_planning_feeds_terminal_image_commands_without_window() {
        let mut widget = TerminalWidget::default();
        let surface = TerminalSurface::for_size(
            Vec2::new(80.0, 40.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );
        let frame = RenderFrame {
            cols: 8,
            rows: 2,
            dirty: Dirty::Full,
            colors: FrameColors {
                background: rgb(12, 34, 56),
                foreground: rgb(220, 221, 222),
                cursor: None,
                ..Default::default()
            },
            cursor: None,
            row_dirty: vec![true, true],
            cells: Vec::new(),
            text: Vec::new(),
            images: KittyImageFrame {
                placements: vec![KittyImagePlacement {
                    image_id: 1,
                    placement_id: 1,
                    layer: KittyImageLayer::BelowText,
                    image_width: 1,
                    image_height: 1,
                    image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
                    source: libghostty_vt::kitty::graphics::SourceRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    destination: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0),
                    data: Arc::new(vec![255, 0, 0, 255]),
                }],
                ..Default::default()
            },
            scrollbar: None,
            stats: Default::default(),
        };
        let plan = widget.planner.plan(surface, &frame, DEFAULT_FONT_SIZE);
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(plan, &TerminalTextConfig::default());
        let render_frame =
            TerminalRenderFrame::from_plan_and_images(plan, &text_contract, &frame.images);

        assert!(
            render_frame
                .commands
                .iter()
                .any(|command| matches!(command, TerminalRenderCommand::Image(_)))
        );
    }

    #[test]
    fn widget_planning_preserves_kitty_storage_deletions_without_window() {
        let mut widget = TerminalWidget::default();
        let surface = TerminalSurface::for_size(
            Vec2::new(80.0, 40.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );
        let mut engine = TerminalEngine::new(TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        })
        .expect("terminal engine");

        engine.write_vt(b"\x1b_Ga=T,t=d,i=51,p=1,s=1,v=1;/////w==\x1b\\");
        engine.write_vt(b"\x1b_Ga=p,i=51,p=2,q=1\x1b\\");
        engine.write_vt(b"\x1b_Ga=d,d=i,i=51,p=1\x1b\\");
        let frame = engine.extract_frame().expect("kitty storage frame");

        let plan = widget.planner.plan(surface, frame, DEFAULT_FONT_SIZE);
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(plan, &TerminalTextConfig::default());
        let render_frame =
            TerminalRenderFrame::from_plan_and_images(plan, &text_contract, &frame.images);

        assert!(render_frame.commands.iter().any(
            |command| matches!(command, TerminalRenderCommand::Image(image)
                if image.image_id == 51 && image.placement_id == 2)
        ));
        assert!(!render_frame.commands.iter().any(
            |command| matches!(command, TerminalRenderCommand::Image(image)
                if image.image_id == 51 && image.placement_id == 1)
        ));
    }

    #[test]
    fn widget_planning_preserves_kitty_rgb_image_load_without_window() {
        let mut widget = TerminalWidget::default();
        let surface = TerminalSurface::for_size(
            Vec2::new(80.0, 40.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );
        let mut engine = TerminalEngine::new(TerminalGeometry {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        })
        .expect("terminal engine");

        engine.write_vt(b"\x1b_Ga=T,f=24,t=d,i=72,p=1,s=1,v=1;AAAA\x1b\\");
        let frame = engine.extract_frame().expect("kitty RGB image frame");
        let plan = widget.planner.plan(surface, frame, DEFAULT_FONT_SIZE);
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(plan, &TerminalTextConfig::default());
        let render_frame =
            TerminalRenderFrame::from_plan_and_images(plan, &text_contract, &frame.images);

        assert!(render_frame.commands.iter().any(
            |command| matches!(command, TerminalRenderCommand::Image(image)
                if image.image_id == 72
                    && image.image_format == libghostty_vt::kitty::graphics::ImageFormat::Rgb
                    && image.data.len() == 3)
        ));
    }

    #[test]
    fn terminal_text_config_preserves_configurable_font_settings() {
        let base = TerminalTextConfig {
            families: vec!["Configured Mono".to_owned(), "Symbols".to_owned()],
            font_features: crate::terminal_text::default_font_features(),
            codepoint_overrides: crate::terminal_text::CodepointFontMap::default(),
            font_size: 15.0,
            cell_width: 9.0,
            cell_height: 21.0,
            baseline_adjustment: -1.0,
            underline_position: 3.0,
            underline_thickness: 2.0,
        };
        let plan = TerminalPaintPlan::default();

        let config = terminal_text_config_for_plan(&plan, &base);

        assert_eq!(config, base);
    }

    #[test]
    fn with_text_config_refreshes_cached_cell_metrics() {
        let config = TerminalTextConfig {
            font_size: 31.0,
            ..TerminalTextConfig::default()
        };
        let expected = terminal_text_cell_metrics(&config).rounded_size();

        let widget = TerminalWidget::new(None).with_text_config(config);

        assert_eq!(widget.cell_size(), expected);
    }

    #[test]
    fn render_cache_reuses_static_commands_and_updates_cursor_alpha() {
        let surface = TerminalSurface::for_size(
            Vec2::new(80.0, 40.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );
        let frame = Arc::new(RenderFrame::default());
        let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0);
        let mut cache = TerminalRenderCache::default();
        cache.store(
            surface,
            &frame,
            TerminalRenderFrame {
                surface: rect,
                commands: vec![
                    TerminalRenderCommand::FillRect(FillCommand {
                        rect,
                        color: PlanColor {
                            r: 1,
                            g: 2,
                            b: 3,
                            a: 255,
                        },
                        role: FillRole::SurfaceBackground,
                    }),
                    TerminalRenderCommand::Cursor(CursorCommand {
                        rect,
                        fill_rect: rect,
                        color: PlanColor {
                            r: 4,
                            g: 5,
                            b: 6,
                            a: 255,
                        },
                        shape: CursorShape::Block,
                    }),
                ],
            },
            9,
        );

        cache.apply_cursor_phase(CursorBlinkPhase::hidden());
        assert_eq!(cache.render_frame().commands.len(), 1);
        assert!(cache.matches(surface, &frame));
        assert_eq!(cache.text_runs(), 9);

        cache.apply_cursor_phase(CursorBlinkPhase::from_opacity(0.5));

        assert!(matches!(
            cache.render_frame().commands.as_slice(),
            [
                TerminalRenderCommand::FillRect(_),
                TerminalRenderCommand::Cursor(cursor)
            ] if cursor.color.a == 128
        ));
    }

    #[test]
    fn terminal_render_shape_requires_wgpu_target_format() {
        let plan = TerminalPaintPlan {
            surface: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0),
            default_background: PlanColor {
                r: 1,
                g: 2,
                b: 3,
                a: 255,
            },
            backgrounds: Vec::new(),
            text_runs: Vec::new(),
            decorations: Vec::new(),
            cursor: None,
        };
        let frame = TerminalRenderFrame::background_from_plan(&plan);

        assert!(terminal_render_shape(&frame, None).is_none());
        assert!(terminal_render_shape(&frame, Some(wgpu::TextureFormat::Rgba8Unorm)).is_some());
    }

    #[test]
    fn cursor_blink_clock_samples_smooth_opacity_curve() {
        let mut clock = CursorBlinkClock::default();
        let start = Instant::now();
        let cursor = cursor_at(1, 0, true);

        assert_eq!(clock.phase(start, Some(cursor)).opacity(), 1.0);
        assert!(
            (clock
                .phase(start + CURSOR_BLINK_PERIOD / 4, Some(cursor))
                .opacity()
                - 0.5)
                .abs()
                < 0.01
        );
        assert!(
            clock
                .phase(start + CURSOR_BLINK_PERIOD / 2, Some(cursor))
                .opacity()
                < 0.01
        );
        assert!(
            (clock
                .phase(start + CURSOR_BLINK_PERIOD * 3 / 4, Some(cursor))
                .opacity()
                - 0.5)
                .abs()
                < 0.01
        );
        assert!(
            (clock
                .phase(start + CURSOR_BLINK_PERIOD, Some(cursor))
                .opacity()
                - 1.0)
                .abs()
                < 0.01
        );
    }

    #[test]
    fn cursor_blink_clock_resets_when_cursor_stops_blinking() {
        let mut clock = CursorBlinkClock::default();
        let start = Instant::now();
        let cursor = cursor_at(1, 0, true);

        assert_eq!(clock.phase(start, Some(cursor)).opacity(), 1.0);
        assert!(
            clock
                .phase(start + CURSOR_BLINK_PERIOD / 2, Some(cursor))
                .opacity()
                < 0.01
        );
        let not_blinking = cursor_at(1, 0, false);
        assert_eq!(
            clock
                .phase(start + CURSOR_BLINK_PERIOD / 2, Some(not_blinking))
                .opacity(),
            1.0
        );
        assert_eq!(
            clock
                .phase(start + CURSOR_BLINK_PERIOD * 3 / 2, Some(cursor))
                .opacity(),
            1.0
        );
    }

    #[test]
    fn cursor_blink_clock_resets_when_cursor_moves() {
        let mut clock = CursorBlinkClock::default();
        let start = Instant::now();
        let first = cursor_at(1, 0, true);
        let moved = cursor_at(2, 0, true);

        assert_eq!(clock.phase(start, Some(first)).opacity(), 1.0);
        assert!(
            clock
                .phase(start + CURSOR_BLINK_PERIOD / 2, Some(first))
                .opacity()
                < 0.01
        );
        assert_eq!(
            clock
                .phase(start + CURSOR_BLINK_PERIOD / 2, Some(moved))
                .opacity(),
            1.0
        );
    }
}
