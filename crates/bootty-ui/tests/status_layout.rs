use bootty_item::ModuleItem;
use bootty_ui::status_layout::{
    Align, ResolvedItem, ResolvedSegment, STATUS_NOTCH_WRAP_HYSTERESIS, item_width,
    status_bar_layout, status_bar_layout_with_tab_wrap,
};
use eframe::egui::{self, Pos2, RawInput, Rect};

fn tab(anchor: &str, text: &str) -> ModuleItem {
    ModuleItem {
        text: text.to_owned(),
        reorder_anchor: Some(anchor.to_owned()),
        ..ModuleItem::default()
    }
}

fn resolved(item: &ModuleItem) -> ResolvedItem<'_> {
    ResolvedItem {
        item,
        icon: None,
        fg: None,
        bg: None,
        stroke: None,
    }
}

/// `run_complete` says the tabs a row asked for were all placed, which is what earns that row its
/// rounded trailing edge. Only a row-ending tab can carry it, and a row the bar's edge cut short
/// must not — a half-drawn tab would otherwise read as a closed one.
#[test]
fn only_a_row_ending_tab_reports_a_complete_run() {
    let items = (0..8)
        .map(|index| tab(&format!("w{index}"), &format!("{index} window")))
        .collect::<Vec<_>>();
    let segments = [ResolvedSegment {
        align: Align::Left,
        wrappable: true,
        source_slot: 0,
        module: "windows.luau",
        surface: "windows",
        items: items.iter().map(resolved).collect(),
        ..ResolvedSegment::default()
    }];

    let context = egui::Context::default();
    context
        .run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 200.0))),
                ..RawInput::default()
            },
            |ui| {
                let bar = Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 30.0));
                // A notch over the leading tabs forces the strip onto more than one row.
                let layout = status_bar_layout(ui, bar, &segments, 0.0, Some((40.0, 200.0)));
                assert!(layout.row_count > 1, "the notch must force a second row");
                for (index, placed) in layout.items.iter().enumerate() {
                    let ends_a_row = layout
                        .items
                        .get(index + 1)
                        .is_none_or(|next| next.run_start);
                    assert!(
                        !placed.run_complete || ends_a_row,
                        "tab {index} claims a complete run mid-row"
                    );
                }
                assert!(
                    layout.items.iter().any(|placed| placed.run_complete),
                    "at least one row placed all of its tabs"
                );
            },
        )
        .drop_without_applying_deltas();
}

#[test]
fn wrapped_tabs_need_clearance_before_collapsing() {
    let items = [tab("w0", "window")];
    let segments = [ResolvedSegment {
        align: Align::Left,
        wrappable: true,
        items: items.iter().map(resolved).collect(),
        ..ResolvedSegment::default()
    }];
    let context = egui::Context::default();

    context
        .run_ui(RawInput::default(), |ui| {
            let bar = Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 30.0));
            let width = item_width(ui, &segments[0].items[0], &egui::FontId::monospace(12.0));
            let notch_right = width + 40.0;

            assert_eq!(
                status_bar_layout_with_tab_wrap(
                    ui,
                    bar,
                    &segments,
                    0.0,
                    Some((width - 1.0, notch_right)),
                    false,
                )
                .row_count(),
                2
            );
            assert_eq!(
                status_bar_layout_with_tab_wrap(
                    ui,
                    bar,
                    &segments,
                    0.0,
                    Some((width + 2.0, notch_right)),
                    true,
                )
                .row_count(),
                2,
                "a wrapped tab stays wrapped inside the hysteresis window"
            );
            assert_eq!(
                status_bar_layout_with_tab_wrap(
                    ui,
                    bar,
                    &segments,
                    0.0,
                    Some((width + STATUS_NOTCH_WRAP_HYSTERESIS + 1.0, notch_right)),
                    true,
                )
                .row_count(),
                1,
                "tabs collapse after clearing the notch by the hysteresis margin"
            );
        })
        .drop_without_applying_deltas();
}
