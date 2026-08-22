use std::collections::HashMap;

use bootty_extension::{ExtensionUiAction, ModuleItem, ModulePrimitive, PublishedSurfaceItem};
use bootty_mux::{controller::MuxScope, snapshot::MuxSession};
use eframe::egui::Color32;

use bootty_ui::item_paint::module_color32;

use crate::ui::session_navigation::BindingSessionGroup;

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarItem<'a> {
    pub id: &'a str,
    pub text: &'a str,
    pub number: Option<usize>,
    pub indent: u16,
    pub tree: Option<&'a str>,
    pub selectable: bool,
    pub session_id: Option<&'a str>,
    pub scope: MuxScope,
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

pub fn build_binding_sidebar_items<'a>(groups: &'a [BindingSessionGroup]) -> Vec<SidebarItem<'a>> {
    let mut items = Vec::new();
    for group in groups {
        items.push(SidebarItem {
            id: &group.label,
            text: &group.label,
            number: None,
            indent: 0,
            tree: None,
            selectable: false,
            session_id: None,
            scope: group.scope,
            reorder_anchor: None,
            color: Color32::WHITE,
            dim_color: Color32::GRAY,
            kind: "group",
            active: false,
            current: false,
            can_return_to_last_session: false,
            context_position: None,
            icon: Some("terminal"),
            primitives: &[],
            extension_action: None,
        });
        let mut binding_items = build_sidebar_items_inner(group);
        for item in &mut binding_items {
            item.indent = item.indent.saturating_add(2);
            item.reorder_anchor = None;
        }
        items.extend(binding_items);
    }
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
    scope: MuxScope,
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
    scope: MuxScope,
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

fn build_sidebar_items_inner<'a>(binding: &'a BindingSessionGroup) -> Vec<SidebarItem<'a>> {
    let sessions = &binding.sessions;
    let scope = binding.scope;
    let mut group_meta = GroupMeta::new(
        sessions,
        sessions.iter().map(|session| binding.display_name(session)),
    );
    let group_count = group_meta.groups.len();
    let full_capacity = sessions.len().saturating_mul(2);
    let mut items = Vec::with_capacity(full_capacity);
    let mut last_group = "";

    for (index, session) in sessions.iter().enumerate() {
        let (group_index, group_info) = group_meta.session(index);
        // Labels come from the name bootty shows; anchors and ids stay on the backend's.
        let display_name = binding.display_name(session);
        let group = group_info.name;
        let group_total = if group.is_empty() {
            0
        } else {
            group_info.count
        };
        let is_grouped = group_total > 1;
        let is_last_in_group =
            is_grouped && group_meta.session_groups.get(index + 1).copied() != Some(group_index);
        let session_tree = if !is_grouped {
            None
        } else if is_last_in_group {
            Some("last")
        } else {
            Some("middle")
        };

        let (color, dim_color) =
            computed_color(group_index, group_count, group_info.position, group_total);
        let selected = binding.session_is_current(session);
        let reorder_anchor = if is_grouped {
            group_info.leader_session
        } else {
            session.name.as_str()
        };
        let (text, number, indent) = if is_grouped {
            if group != last_group {
                items.push(SidebarItem {
                    id: group,
                    text: group,
                    number: None,
                    indent: 0,
                    tree: None,
                    selectable: false,
                    session_id: None,
                    scope,
                    reorder_anchor: Some(reorder_anchor),
                    color,
                    dim_color,
                    kind: "group",
                    active: false,
                    current: false,
                    can_return_to_last_session: false,
                    context_position: None,
                    icon: None,
                    primitives: &[],
                    extension_action: None,
                });
            }
            let suffix = session_suffix(display_name);
            let label = if suffix.is_empty() { group } else { suffix };
            (label, Some(index + 1), 2)
        } else {
            let label = if group.is_empty() {
                display_name
            } else {
                group
            };
            (label, Some(index + 1), 0)
        };

        items.push(SidebarItem {
            id: session.id.as_str(),
            text,
            number,
            indent,
            tree: session_tree,
            selectable: true,
            session_id: Some(session.id.as_str()),
            scope,
            reorder_anchor: Some(reorder_anchor),
            color,
            dim_color,
            kind: "session",
            active: selected,
            current: selected,
            can_return_to_last_session: binding.can_return_to_last_session,
            context_position: Some((index, sessions.len())),
            icon: None,
            primitives: &[],
            extension_action: None,
        });
        last_group = group;
    }

    items
}

pub fn session_group(name: &str) -> &str {
    name.split_once('/').map_or(name, |(group, _)| group)
}

pub fn session_suffix(name: &str) -> &str {
    name.split_once('/').map_or("", |(_, suffix)| suffix)
}

#[derive(Clone, Copy, Debug)]
struct Group<'a> {
    name: &'a str,
    leader_session: &'a str,
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
    /// in which case the backend name stands in. `leader_session` stays a backend name: it is the
    /// reorder anchor, an identity rather than a label.
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
                leader_session: session.name.as_str(),
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
