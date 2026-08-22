use std::collections::HashMap;

use bootty_extension::{ExtensionUiAction, ModuleItem, ModulePrimitive, PublishedSurfaceItem};
use bootty_mux::{controller::SpaceId, snapshot::MuxSession};
use eframe::egui::Color32;

use bootty_ui::item_paint::module_color32;

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarItem<'a> {
    pub id: &'a str,
    pub text: &'a str,
    pub number: Option<usize>,
    pub indent: u16,
    pub tree: Option<&'a str>,
    pub selectable: bool,
    pub session_id: Option<&'a str>,
    pub scope: SpaceId,
    pub reorder_anchor: Option<&'a str>,
    pub color: Color32,
    pub dim_color: Color32,
    pub kind: &'a str,
    pub active: bool,
    pub current: bool,
    pub can_return_to_last_session: bool,
    pub context_position: Option<(usize, usize)>,
    pub icon: Option<&'a str>,
    pub primitives: &'a [ModulePrimitive],
    pub extension_action: Option<ExtensionUiAction>,
}

/// A session on this Space's multiplexer that no Space claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnclaimedSession {
    pub session_id: String,
    pub name: String,
}

/// The trailing "Unassigned" block, empty when nothing is unclaimed.
///
/// Sessions started outside bootty, and ones a deleted Space left behind, are otherwise invisible
/// here -- membership is explicit now, so the sessions it does not cover have to be somewhere.
pub fn unassigned_sidebar_items<'a>(
    sessions: &'a [UnclaimedSession],
    scope: SpaceId,
) -> Vec<SidebarItem<'a>> {
    if sessions.is_empty() {
        return Vec::new();
    }
    let mut items = vec![SidebarItem {
        id: "unassigned",
        text: "Unassigned",
        number: None,
        indent: 0,
        tree: None,
        selectable: false,
        session_id: None,
        scope,
        reorder_anchor: None,
        color: Color32::WHITE,
        dim_color: Color32::GRAY,
        kind: "group",
        active: false,
        current: false,
        can_return_to_last_session: false,
        context_position: None,
        icon: Some("circle-dashed"),
        primitives: &[],
        extension_action: None,
    }];
    items.extend(sessions.iter().map(|session| SidebarItem {
        id: &session.session_id,
        text: &session.name,
        number: None,
        indent: 2,
        tree: None,
        selectable: true,
        session_id: Some(&session.session_id),
        scope,
        reorder_anchor: None,
        color: Color32::WHITE,
        dim_color: Color32::GRAY,
        kind: crate::ui::chrome::UNASSIGNED_KIND,
        active: false,
        current: false,
        can_return_to_last_session: false,
        context_position: None,
        icon: Some("terminal"),
        primitives: &[],
        extension_action: None,
    }));
    items
}

/// Session accents, grouped the same way the sidebar groups its rows: by the names bootty shows.
pub fn sidebar_session_colors<'a, T: AsRef<str>>(
    sessions: &'a [MuxSession],
    display_names: &'a [T],
) -> Vec<(Color32, Color32)> {
    let mut group_meta = GroupMeta::new(sessions, display_names.iter().map(|name| name.as_ref()));
    let dynamic_total = group_meta.groups.len();
    sessions
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let (group_index, group) = group_meta.session(index);
            let group_total = if group.name.is_empty() {
                0
            } else {
                group.count
            };
            let (color, dim_color) =
                computed_color(group_index, dynamic_total, group.position, group_total);
            (color, dim_color)
        })
        .collect()
}

pub fn build_sidebar_items_from_published_items<'a>(
    items: &'a [PublishedSurfaceItem],
    scope: SpaceId,
    selected_session: Option<&str>,
    can_return_to_last_session: bool,
) -> Vec<SidebarItem<'a>> {
    let mut rows = items
        .iter()
        .filter_map(|published| {
            sidebar_item_from_module_item(
                &published.item,
                published.action(),
                scope,
                selected_session,
                can_return_to_last_session,
            )
        })
        .collect::<Vec<_>>();
    let mut positions = HashMap::new();
    for id in rows
        .iter()
        .filter(|row| row.kind == "session")
        .filter_map(|row| row.session_id)
    {
        let position = positions.len();
        positions.entry(id).or_insert(position);
    }
    let count = positions.len();
    for row in &mut rows {
        row.context_position = row
            .session_id
            .and_then(|id| positions.get(id).copied())
            .map(|position| (position, count));
    }
    rows
}

fn sidebar_item_from_module_item<'a>(
    item: &'a ModuleItem,
    extension_action: Option<ExtensionUiAction>,
    scope: SpaceId,
    selected_session: Option<&str>,
    can_return_to_last_session: bool,
) -> Option<SidebarItem<'a>> {
    let kind = item.kind.as_deref().unwrap_or("row");
    if kind == "footer" {
        return None;
    }
    let row_key = item.key.as_deref().unwrap_or_else(|| {
        if kind == "session" {
            item.session_id.as_deref().unwrap_or(item.text.as_str())
        } else {
            item.text.as_str()
        }
    });
    let selected = selected_session.is_some_and(|selected| {
        item.session_id.as_deref() == Some(selected) || item.text == selected
    });
    let selectable = item.selectable.unwrap_or(kind == "session");
    let current = if selectable && selected_session.is_some() {
        selected
    } else {
        item.current.unwrap_or(false)
    };
    let color = item.fg.map(module_color32).unwrap_or(Color32::WHITE);
    Some(SidebarItem {
        id: row_key,
        text: item.text.as_str(),
        number: item.number,
        indent: item.indent.unwrap_or(0),
        tree: item.tree.as_deref(),
        selectable,
        session_id: item.session_id.as_deref(),
        scope,
        reorder_anchor: item.reorder_anchor.as_deref(),
        color,
        dim_color: item.dim_fg.map(module_color32).unwrap_or(color),
        kind,
        active: kind == "session"
            && selected_session.map_or(item.active.unwrap_or(current), |_| current),
        current,
        can_return_to_last_session: item.session_id.is_some() && can_return_to_last_session,
        context_position: None,
        icon: item.icon.as_deref(),
        primitives: &item.primitives,
        extension_action,
    })
}

pub fn session_group(name: &str) -> &str {
    name.split_once('/').map_or(name, |(group, _)| group)
}

#[derive(Clone, Copy, Debug)]
struct Group<'a> {
    name: &'a str,
    count: usize,
    position: usize,
}

struct GroupMeta<'a> {
    groups: Vec<Group<'a>>,
    session_groups: Vec<usize>,
}

impl<'a> GroupMeta<'a> {
    /// Groups sessions by the name bootty shows, so a backend-only uniqueness suffix cannot split a
    /// project into two groups. `display_names` pairs with `sessions` by position and may be short,
    /// in which case the backend name stands in.
    fn new(
        display_sessions: &'a [MuxSession],
        mut display_names: impl Iterator<Item = &'a str>,
    ) -> Self {
        let mut groups = Vec::<Group<'a>>::new();
        let mut session_groups = Vec::with_capacity(display_sessions.len());
        let mut lookup = HashMap::<&'a str, usize>::new();
        for session in display_sessions {
            let group = session_group(display_names.next().unwrap_or(session.name.as_str()));
            if let Some(index) = lookup.get(group).copied() {
                groups[index].count += 1;
                session_groups.push(index);
                continue;
            }

            let index = groups.len();
            groups.push(Group {
                name: group,
                count: 1,
                position: 0,
            });
            session_groups.push(index);
            lookup.insert(group, index);
        }
        Self {
            groups,
            session_groups,
        }
    }

    fn session(&mut self, index: usize) -> (usize, Group<'a>) {
        let group_index = self.session_groups[index];
        let summary = &mut self.groups[group_index];
        let position = summary.position;
        if !summary.name.is_empty() {
            summary.position += 1;
        }
        (
            group_index,
            Group {
                position,
                ..*summary
            },
        )
    }
}

fn computed_color(
    pos: usize,
    total: usize,
    group_pos: usize,
    group_total: usize,
) -> (Color32, Color32) {
    let base = if total > 0 {
        60.0 + (pos as f64 * 300.0) / total as f64
    } else {
        210.0
    };
    let (hue, lightness) = if group_total > 1 {
        let t = group_pos as f64 / (group_total - 1) as f64;
        (
            (base + (t * 60.0 - 30.0) + 360.0) % 360.0,
            0.55 + (t - 0.5) * 0.15,
        )
    } else {
        (base, 0.6)
    };
    (
        hsl_to_color(hue, 0.55, lightness),
        hsl_to_color(hue, 0.2, 0.45),
    )
}

fn hsl_to_color(hue: f64, saturation: f64, lightness: f64) -> Color32 {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hp = hue / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let m = lightness - c / 2.0;
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let channel = |value| ((value + m) * 255.0) as u8;
    Color32::from_rgb(channel(r), channel(g), channel(b))
}
