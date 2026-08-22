use std::collections::HashMap;

use bootty_extension::{ExtensionUiAction, ModuleItem, ModulePrimitive, PublishedSurfaceItem};
use bootty_mux::{controller::MuxScope, snapshot::MuxSession};
use eframe::egui::Color32;

use crate::{
    command_extensions::{ExtensionUiAction, ModuleItem, ModulePrimitive, PublishedSurfaceItem},
    mux::{controller::MuxScope, snapshot::MuxSession},
    ui::session_navigation::BindingSessionGroup,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarState {
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SidebarItemKind {
    Group,
    Session { active: bool },
    Row,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarItemId<'a> {
    Binding(MuxScope),
    Group { scope: MuxScope, name: &'a str },
    Session { scope: MuxScope, id: &'a str },
    Row(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarDisplay<'a> {
    Text(&'a str),
    Numbered { number: usize, label: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTree {
    None,
    Middle,
    Last,
    Pipe,
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarItem<'a> {
    pub id: SidebarItemId<'a>,
    pub display: SidebarDisplay<'a>,
    pub indent: u16,
    pub tree: SidebarTree,
    pub selectable: bool,
    pub session_id: Option<&'a str>,
    pub session_scope: Option<MuxScope>,
    pub reorder_anchor: Option<&'a str>,
    pub color: Color32,
    pub dim_color: Color32,
    pub kind: SidebarItemKind,
    pub current: bool,
    pub can_return_to_last_session: bool,
    pub icon: Option<&'a str>,
    pub primitives: &'a [ModulePrimitive],
    pub extension_action: Option<ExtensionUiAction>,
}

pub fn build_binding_sidebar_items<'a>(groups: &'a [BindingSessionGroup]) -> Vec<SidebarItem<'a>> {
    let mut items = Vec::new();
    for group in groups {
        items.push(SidebarItem {
            id: SidebarItemId::Binding(group.scope),
            display: SidebarDisplay::Text(&group.label),
            indent: 0,
            tree: SidebarTree::None,
            selectable: false,
            session_id: None,
            session_scope: None,
            reorder_anchor: None,
            color: Color32::WHITE,
            dim_color: Color32::GRAY,
            kind: SidebarItemKind::Group,
            current: false,
            can_return_to_last_session: false,
            icon: Some("terminal"),
            primitives: &[],
            extension_action: None,
        });
        let display_names = group
            .sessions
            .iter()
            .map(|session| group.display_name(session))
            .collect::<Vec<_>>();
        let mut binding_items = build_sidebar_items_inner(
            group.scope,
            &group.sessions,
            &display_names,
            group.selected_session.as_deref(),
            group.active,
            group.can_return_to_last_session,
        );
        for item in &mut binding_items {
            item.indent = item.indent.saturating_add(2);
            item.reorder_anchor = None;
        }
        items.extend(binding_items);
    }
    items
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarSessionColor<'a> {
    pub session_id: &'a str,
    pub color: Color32,
    pub dim_color: Color32,
}

/// Session accents, grouped the same way the sidebar groups its rows: by the names bootty shows.
pub fn sidebar_session_colors<'a>(
    sessions: &'a [MuxSession],
    display_names: &[&'a str],
) -> Vec<SidebarSessionColor<'a>> {
    let mut group_meta = GroupMeta::new(sessions, display_names);
    let dynamic_total = group_meta.dynamic_total;
    sessions
        .iter()
        .enumerate()
        .filter_map(|(index, session)| {
            let group_info = group_meta.session(index)?;
            let group_total = if group_info.name.is_empty() {
                0
            } else {
                group_info.count
            };
            let (color, dim_color) = computed_color(
                group_info.index,
                dynamic_total,
                group_info.position,
                group_total,
            );
            Some(SidebarSessionColor {
                session_id: session.id.as_str(),
                color,
                dim_color,
            })
        })
        .collect()
}

pub fn build_sidebar_items_from_published_items<'a>(
    items: &'a [PublishedSurfaceItem],
    scope: MuxScope,
    selected_session: Option<&str>,
    can_return_to_last_session: bool,
) -> Vec<SidebarItem<'a>> {
    items
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
        .collect()
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
    let display = if let Some(number) = item.number {
        SidebarDisplay::Numbered {
            number,
            label: item.text.as_str(),
        }
    } else {
        SidebarDisplay::Text(item.text.as_str())
    };
    let selected = selected_session.is_some_and(|selected| {
        item.session_id.as_deref() == Some(selected) || item.text == selected
    });
    let selectable = item.selectable.unwrap_or(kind == "session");
    let current = if selectable && selected_session.is_some() {
        selected
    } else {
        item.current.unwrap_or(false)
    };
    let sidebar_kind = match kind {
        "group" => SidebarItemKind::Group,
        "session" => SidebarItemKind::Session {
            active: selected_session.map_or(item.active.unwrap_or(current), |_| current),
        },
        _ => SidebarItemKind::Row,
    };
    let color = item.fg.map(module_color32).unwrap_or(Color32::WHITE);
    Some(SidebarItem {
        id: sidebar_item_id(kind, scope, row_key, item.text.as_str()),
        display,
        indent: item.indent.unwrap_or(0),
        tree: sidebar_tree(item.tree.as_deref()),
        selectable,
        session_id: item.session_id.as_deref(),
        session_scope: item.session_id.as_ref().map(|_| scope),
        reorder_anchor: item.reorder_anchor.as_deref(),
        color,
        dim_color: item.dim_fg.map(module_color32).unwrap_or(color),
        kind: sidebar_kind,
        current,
        // Every row a session owns offers the same context menu, so the flag follows the session
        // rather than the title row alone.
        can_return_to_last_session: item.session_id.is_some() && can_return_to_last_session,
        icon: item.icon.as_deref(),
        primitives: &item.primitives,
        extension_action,
    })
}

fn sidebar_item_id<'a>(
    kind: &str,
    scope: MuxScope,
    row_key: &'a str,
    text: &'a str,
) -> SidebarItemId<'a> {
    match kind {
        "group" => SidebarItemId::Group { scope, name: text },
        "session" => SidebarItemId::Session { scope, id: row_key },
        _ => SidebarItemId::Row(row_key),
    }
}

fn sidebar_tree(value: Option<&str>) -> SidebarTree {
    match value {
        Some("middle") => SidebarTree::Middle,
        Some("last") => SidebarTree::Last,
        Some("pipe") => SidebarTree::Pipe,
        Some("blank") => SidebarTree::Blank,
        _ => SidebarTree::None,
    }
}

fn build_sidebar_items_inner<'a>(
    scope: MuxScope,
    sessions: &'a [MuxSession],
    display_names: &[&'a str],
    selected_session: Option<&str>,
    binding_active: bool,
    can_return_to_last_session: bool,
) -> Vec<SidebarItem<'a>> {
    let mut group_meta = GroupMeta::new(sessions, display_names);
    let full_capacity = sessions.len().saturating_mul(6);
    let mut items = Vec::with_capacity(full_capacity);
    let mut ordinal = 0usize;
    let mut last_group = "";

    for (index, session) in sessions.iter().enumerate() {
        let Some(group_info) = group_meta.session(index) else {
            continue;
        };
        // Labels come from the name bootty shows; anchors and ids stay on the backend's.
        let display_name = display_names
            .get(index)
            .copied()
            .unwrap_or(session.name.as_str());
        let group = group_info.name;
        let group_index = group_info.index;
        let group_count = group_info.count;
        let group_total = if group.is_empty() { 0 } else { group_count };
        let is_grouped = !group.is_empty() && group_total > 1;
        let is_last_in_group =
            is_grouped && group_meta.session_group_index(index + 1) != Some(group_index);
        let session_tree = if !is_grouped {
            SidebarTree::None
        } else if is_last_in_group {
            SidebarTree::Last
        } else {
            SidebarTree::Middle
        };

        let (color, dim_color) = computed_color(
            group_index,
            group_meta.dynamic_total,
            group_info.position,
            group_total,
        );
        let selected = binding_active
            && if selected_session.is_some() {
                selected_session == Some(session.id.as_str())
                    || selected_session == Some(session.name.as_str())
            } else {
                session.active
            };
        let reorder_anchor = if is_grouped {
            group_info.leader_session
        } else {
            session.name.as_str()
        };
        let (display, session_indent) = if is_grouped {
            if group != last_group {
                items.push(SidebarItem {
                    id: SidebarItemId::Group { scope, name: group },
                    display: SidebarDisplay::Text(group),
                    indent: 0,
                    tree: SidebarTree::None,
                    selectable: false,
                    session_id: None,
                    session_scope: None,
                    reorder_anchor: Some(reorder_anchor),
                    color,
                    dim_color,
                    kind: SidebarItemKind::Group,
                    current: false,
                    can_return_to_last_session: false,
                    icon: None,
                    primitives: &[],
                    extension_action: None,
                });
            }
            let suffix = session_suffix(display_name);
            let label = if suffix.is_empty() { group } else { suffix };
            let display = SidebarDisplay::Numbered {
                number: ordinal + 1,
                label,
            };
            ordinal += 1;
            (display, 2)
        } else {
            let label = if group.is_empty() {
                display_name
            } else {
                group
            };
            let display = SidebarDisplay::Numbered {
                number: ordinal + 1,
                label,
            };
            ordinal += 1;
            (display, 0)
        };

        items.push(SidebarItem {
            id: SidebarItemId::Session {
                scope,
                id: session.id.as_str(),
            },
            display,
            indent: session_indent,
            tree: session_tree,
            selectable: true,
            session_id: Some(session.id.as_str()),
            session_scope: Some(scope),
            reorder_anchor: Some(reorder_anchor),
            color,
            dim_color,
            kind: SidebarItemKind::Session { active: selected },
            current: selected,
            can_return_to_last_session,
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

#[derive(Debug)]
struct GroupSummary<'a> {
    name: &'a str,
    leader_session: &'a str,
    count: usize,
    position: usize,
}

#[derive(Debug)]
struct GroupSession<'a> {
    name: &'a str,
    leader_session: &'a str,
    index: usize,
    count: usize,
    position: usize,
}

struct GroupMeta<'a> {
    groups: Vec<GroupSummary<'a>>,
    session_groups: Vec<usize>,
    dynamic_total: usize,
}

impl<'a> GroupMeta<'a> {
    /// Groups sessions by the name bootty shows, so a backend-only uniqueness suffix cannot split a
    /// project into two groups. `display_names` pairs with `sessions` by position and may be short,
    /// in which case the backend name stands in. `leader_session` stays a backend name: it is the
    /// reorder anchor, an identity rather than a label.
    fn new(sessions: &'a [MuxSession], display_names: &[&'a str]) -> Self {
        let mut groups = Vec::<GroupSummary<'a>>::new();
        let mut session_groups = Vec::with_capacity(sessions.len());
        let mut lookup = HashMap::<&'a str, usize>::new();
        for (index, session) in sessions.iter().enumerate() {
            let group = session_group(
                display_names
                    .get(index)
                    .copied()
                    .unwrap_or(session.name.as_str()),
            );
            if let Some(index) = lookup.get(group).copied() {
                groups[index].count += 1;
                session_groups.push(index);
                continue;
            }

            let index = groups.len();
            groups.push(GroupSummary {
                name: group,
                leader_session: session.name.as_str(),
                count: 1,
                position: 0,
            });
            session_groups.push(index);
            lookup.insert(group, index);
        }
        let dynamic_total = groups.len();
        Self {
            groups,
            session_groups,
            dynamic_total,
        }
    }

    fn session_group_index(&self, index: usize) -> Option<usize> {
        self.session_groups.get(index).copied()
    }

    fn session(&mut self, index: usize) -> Option<GroupSession<'a>> {
        let group_index = self.session_group_index(index)?;
        let summary = self.groups.get_mut(group_index)?;
        let position = summary.position;
        if !summary.name.is_empty() {
            summary.position += 1;
        }
        Some(GroupSession {
            name: summary.name,
            leader_session: summary.leader_session,
            index: group_index,
            count: summary.count,
            position,
        })
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
    Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}
