pub(super) fn frame(now: std::time::Instant, events: Vec<egui::Event>) -> bootty_app::FrameInputs {
    bootty_app::FrameInputs {
        now,
        events,
        dropped_file_paths: Vec::new(),
        modifiers: egui::Modifiers::NONE,
        hover_pos: None,
        pressed_mouse_button: None,
        viewport: bootty_app::ViewportSnapshot::default(),
        window_focused: true,
        renderer_metrics: bootty_app::renderer::RendererMetrics::default(),
        terminal_cell_width: 9.0,
        terminal_cell_height: 20.0,
        terminal_scale_factor: 1.0,
        terminal_view_transform: bootty_render::geometry::ViewTransform::IDENTITY,
    }
}
