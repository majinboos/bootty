use std::collections::{BTreeMap, HashSet};

use eframe::egui::Color32;

use crate::{
    mux::{
        sidebar_meta::{DiffStat, SidebarMetadata},
        snapshot::MuxSession,
    },
    strings::truncate_label,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarState {
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SidebarItemKind {
    Group,
    Session {
        active: bool,
        process: Option<String>,
        diff: Option<DiffStat>,
    },
    Process {
        name: String,
        cpu_pct: Option<f32>,
        mem_bytes: Option<u64>,
    },
    Agent {
        text: String,
    },
    Branch {
        name: String,
    },
    Status {
        text: String,
    },
    Progress {
        pct: u8,
    },
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
pub struct SidebarItem {
    pub id: String,
    pub display: String,
    pub indent: u16,
    pub tree: SidebarTree,
    pub selectable: bool,
    pub session_id: Option<String>,
    pub color: Color32,
    pub dim_color: Color32,
    pub kind: SidebarItemKind,
    pub current: bool,
}

const NUM_SELECTED: &[char] = &[
    '\u{F03A4}',
    '\u{F03A7}',
    '\u{F03AA}',
    '\u{F03AD}',
    '\u{F03B1}',
    '\u{F03B3}',
    '\u{F03B6}',
    '\u{F03B9}',
    '\u{F03BC}',
    '\u{F03BF}',
];
const NUM_UNSELECTED: &[char] = &[
    '\u{F03A6}',
    '\u{F03A9}',
    '\u{F03AC}',
    '\u{F03AE}',
    '\u{F03B0}',
    '\u{F03B5}',
    '\u{F03B8}',
    '\u{F03BB}',
    '\u{F03BE}',
    '\u{F03C1}',
];

fn num_glyph(index: usize, selected: bool) -> char {
    let table = if selected {
        NUM_SELECTED
    } else {
        NUM_UNSELECTED
    };
    *table.get(index).unwrap_or(&table[table.len() - 1])
}

pub fn build_sidebar_items(
    sessions: &[MuxSession],
    selected_session: Option<&str>,
    metadata: &SidebarMetadata,
) -> Vec<SidebarItem> {
    let names = sessions
        .iter()
        .map(|session| session.name.clone())
        .collect::<Vec<_>>();
    let group_meta = GroupMeta::new(&names);
    let colors = session_colors(&names, &group_meta);
    let color_by_name = colors
        .into_iter()
        .map(|(name, color, dim)| (name, (color, dim)))
        .collect::<BTreeMap<_, _>>();

    let mut items = Vec::new();
    let mut ordinal = 0usize;
    let mut last_group = "";

    for (index, session) in sessions.iter().enumerate() {
        let group = session_group(&session.name);
        let group_total = if group.is_empty() {
            0
        } else {
            *group_meta.counts.get(group).unwrap_or(&0)
        };
        let is_grouped = !group.is_empty() && group_total > 1;
        let is_last_in_group = is_grouped
            && sessions
                .get(index + 1)
                .map(|next| session_group(&next.name))
                != Some(group);
        let session_tree = if !is_grouped {
            SidebarTree::None
        } else if is_last_in_group {
            SidebarTree::Last
        } else {
            SidebarTree::Middle
        };
        let detail_tree = if !is_grouped {
            SidebarTree::None
        } else if is_last_in_group {
            SidebarTree::Blank
        } else {
            SidebarTree::Pipe
        };
        let (color, dim_color) = color_by_name
            .get(&session.name)
            .copied()
            .unwrap_or((Color32::WHITE, Color32::GRAY));

        let selected = if selected_session.is_some() {
            selected_session == Some(session.id.as_str())
                || selected_session == Some(session.name.as_str())
        } else {
            session.active
        };
        let (display, session_indent, detail_indent) = if is_grouped {
            if group != last_group {
                items.push(SidebarItem {
                    id: format!("__group__{group}"),
                    display: group.to_owned(),
                    indent: 0,
                    tree: SidebarTree::None,
                    selectable: false,
                    session_id: None,
                    color,
                    dim_color,
                    kind: SidebarItemKind::Group,
                    current: false,
                });
            }
            let suffix = session_suffix(&session.name);
            let label = if suffix.is_empty() { group } else { suffix };
            let display = format!("{} {label}", num_glyph(ordinal, selected));
            ordinal += 1;
            (display, 2, 4)
        } else {
            let label = if group.is_empty() {
                session.name.as_str()
            } else {
                group
            };
            let display = format!("{} {label}", num_glyph(ordinal, selected));
            ordinal += 1;
            (display, 0, 2)
        };

        let meta = metadata.get(&session.name);
        items.push(SidebarItem {
            id: session.id.clone(),
            display,
            indent: session_indent,
            tree: session_tree,
            selectable: true,
            session_id: Some(session.id.clone()),
            color,
            dim_color,
            kind: SidebarItemKind::Session {
                active: selected,
                process: session.anchor.process.clone(),
                diff: meta.and_then(|meta| meta.diff),
            },
            current: selected,
        });
        if let Some(process) = meta.and_then(|meta| meta.processes.first()) {
            items.push(SidebarItem {
                id: format!("__process__{}", session.id),
                display: process.name.clone(),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.clone()),
                color,
                dim_color,
                kind: SidebarItemKind::Process {
                    name: process.name.clone(),
                    cpu_pct: Some(process.cpu_pct),
                    mem_bytes: Some(process.mem_bytes),
                },
                current: selected,
            });
        } else if let Some(process) = session
            .anchor
            .process
            .as_ref()
            .filter(|process| !process.is_empty())
        {
            items.push(SidebarItem {
                id: format!("__process__{}", session.id),
                display: process.clone(),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.clone()),
                color,
                dim_color,
                kind: SidebarItemKind::Process {
                    name: process.clone(),
                    cpu_pct: meta
                        .and_then(|meta| meta.process_cpu.as_deref())
                        .and_then(parse_cpu_percent),
                    mem_bytes: None,
                },
                current: selected,
            });
        }
        if let Some(agent_status) = meta.and_then(|meta| meta.agent_status.as_ref()) {
            items.push(SidebarItem {
                id: format!("__agent__{}", session.id),
                display: agent_status.clone(),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.clone()),
                color,
                dim_color,
                kind: SidebarItemKind::Agent {
                    text: agent_status.clone(),
                },
                current: selected,
            });
        }
        if let Some(branch) = meta.and_then(|meta| meta.branch.as_ref()) {
            items.push(SidebarItem {
                id: format!("__branch__{}", session.id),
                display: branch.clone(),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.clone()),
                color,
                dim_color,
                kind: SidebarItemKind::Branch {
                    name: branch.clone(),
                },
                current: selected,
            });
        }
        if let Some(status) = meta.and_then(|meta| meta.status.as_ref()) {
            items.push(SidebarItem {
                id: format!("__status__{}", session.id),
                display: status.clone(),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.clone()),
                color,
                dim_color,
                kind: SidebarItemKind::Status {
                    text: status.clone(),
                },
                current: selected,
            });
        }
        if let Some(progress) = meta.and_then(|meta| meta.progress) {
            items.push(SidebarItem {
                id: format!("__progress__{}", session.id),
                display: format!("{progress}%"),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.clone()),
                color,
                dim_color,
                kind: SidebarItemKind::Progress { pct: progress },
                current: selected,
            });
        }

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

pub fn tree_prefix(tree: SidebarTree, indent: u16) -> String {
    let spaces = " ".repeat(indent as usize);
    match tree {
        SidebarTree::None | SidebarTree::Blank => spaces,
        SidebarTree::Middle => format!("├{}", " ".repeat(indent.saturating_sub(1) as usize)),
        SidebarTree::Last => format!("└{}", " ".repeat(indent.saturating_sub(1) as usize)),
        SidebarTree::Pipe => format!("│{}", " ".repeat(indent.saturating_sub(1) as usize)),
    }
}

pub fn item_label(item: &SidebarItem, width: usize) -> String {
    truncate_label(
        &format!("{}{}", tree_prefix(item.tree, item.indent), item.display),
        width,
    )
}

#[derive(Debug)]
struct GroupMeta {
    counts: BTreeMap<String, usize>,
    group_idx: BTreeMap<String, usize>,
    dynamic_total: usize,
}

impl GroupMeta {
    fn new(sessions: &[String]) -> Self {
        let mut counts = BTreeMap::new();
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        for session in sessions {
            let group = session_group(session);
            *counts.entry(group.to_owned()).or_default() += 1;
            if seen.insert(group.to_owned()) {
                order.push(group.to_owned());
            }
        }
        let group_idx = order
            .iter()
            .enumerate()
            .map(|(index, group)| (group.clone(), index))
            .collect::<BTreeMap<_, _>>();
        Self {
            counts,
            group_idx,
            dynamic_total: order.len(),
        }
    }
}

fn session_colors(sessions: &[String], meta: &GroupMeta) -> Vec<(String, Color32, Color32)> {
    let mut group_positions = BTreeMap::<&str, usize>::new();
    let mut result = Vec::with_capacity(sessions.len());
    for session in sessions {
        let group = session_group(session);
        let group_total = if group.is_empty() {
            0
        } else {
            *meta.counts.get(group).unwrap_or(&0)
        };
        let group_pos = *group_positions.get(group).unwrap_or(&0);
        let group_index = *meta.group_idx.get(group).unwrap_or(&0);
        let (color, dim) = computed_color(group_index, meta.dynamic_total, group_pos, group_total);
        if !group.is_empty() {
            *group_positions.entry(group).or_default() += 1;
        }
        result.push((session.clone(), color, dim));
    }
    result
}

fn parse_cpu_percent(value: &str) -> Option<f32> {
    value.trim_end_matches('%').parse().ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::{
        sidebar_meta::{DiffStat, SidebarSessionMetadata},
        snapshot::MuxPaneAnchor,
    };

    #[test]
    fn groups_sessions_and_places_detail_rows_after_session() {
        let sessions = vec![
            session("$1", "work/api", "zsh"),
            session("$2", "work/ui", "nvim"),
        ];
        let mut metadata = SidebarMetadata::default();
        metadata.insert(
            "work/api",
            SidebarSessionMetadata {
                branch: Some("main".to_owned()),
                diff: Some(DiffStat {
                    added: 7,
                    removed: 4,
                }),
                status: Some("review".to_owned()),
                progress: Some(42),
                ..SidebarSessionMetadata::default()
            },
        );

        let items = build_sidebar_items(&sessions, Some("$1"), &metadata);
        assert_ne!(items[1].display, "1 api");

        assert!(items[1].display.ends_with(" api"));
        assert!(matches!(items[1].kind, SidebarItemKind::Session { .. }));
        assert!(matches!(items[2].kind, SidebarItemKind::Process { .. }));
        assert!(matches!(items[3].kind, SidebarItemKind::Branch { .. }));
        assert!(matches!(items[4].kind, SidebarItemKind::Status { .. }));
        assert!(matches!(
            items[5].kind,
            SidebarItemKind::Progress { pct: 42 }
        ));
        assert!(items[6].display.ends_with(" ui"));
        assert_eq!(items[1].tree, SidebarTree::Middle);
        assert_eq!(items[6].tree, SidebarTree::Last);
    }

    #[test]
    fn selected_session_does_not_also_mark_attached_session_current() {
        let mut sessions = vec![session("$1", "one", "zsh"), session("$2", "two", "fish")];
        sessions[0].active = true;

        let items = build_sidebar_items(&sessions, Some("$2"), &SidebarMetadata::default());

        let current = items
            .iter()
            .filter(|item| matches!(item.kind, SidebarItemKind::Session { .. }) && item.current)
            .map(|item| item.session_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(current, vec![Some("$2")]);
    }

    #[test]
    fn session_group_uses_first_slash() {
        assert_eq!(session_group("a/b/c"), "a");
        assert_eq!(session_suffix("a/b/c"), "b/c");
    }

    fn session(id: &str, name: &str, process: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: None,
                cwd: None,
                process: Some(process.to_owned()),
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }
}
