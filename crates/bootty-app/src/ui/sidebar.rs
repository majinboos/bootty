use std::collections::HashMap;

use eframe::egui::Color32;

use crate::mux::{
    sidebar_meta::{DiffStat, SidebarMetadata},
    snapshot::MuxSession,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarState {
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SidebarItemKind<'a> {
    Group,
    Session {
        active: bool,
        process: Option<&'a str>,
        diff: Option<DiffStat>,
    },
    Process {
        name: &'a str,
        cpu_pct: Option<f32>,
        mem_bytes: Option<u64>,
    },
    Agent {
        text: &'a str,
    },
    Branch {
        name: &'a str,
    },
    Status {
        text: &'a str,
    },
    Progress {
        pct: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarItemId<'a> {
    Group(&'a str),
    Session(&'a str),
    Process(&'a str),
    Agent(&'a str),
    Branch(&'a str),
    Status(&'a str),
    Progress(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarDisplay<'a> {
    Text(&'a str),
    Numbered { number: usize, label: &'a str },
    Progress(u8),
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
    pub reorder_anchor: Option<&'a str>,
    pub color: Color32,
    pub dim_color: Color32,
    pub kind: SidebarItemKind<'a>,
    pub current: bool,
}

pub fn build_sidebar_items<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
    metadata: &'a SidebarMetadata,
) -> Vec<SidebarItem<'a>> {
    build_sidebar_items_inner(sessions, selected_session, metadata, None)
}

pub fn build_visible_sidebar_items<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
    metadata: &'a SidebarMetadata,
    max_rows: usize,
) -> Vec<SidebarItem<'a>> {
    build_sidebar_items_inner(sessions, selected_session, metadata, Some(max_rows))
}

fn build_sidebar_items_inner<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
    metadata: &'a SidebarMetadata,
    max_rows: Option<usize>,
) -> Vec<SidebarItem<'a>> {
    if max_rows == Some(0) {
        return Vec::new();
    }
    let mut group_meta = GroupMeta::new(sessions);
    let full_capacity = sessions.len().saturating_mul(6);
    let mut items = Vec::with_capacity(max_rows.unwrap_or(full_capacity).min(full_capacity));
    let mut ordinal = 0usize;
    let mut last_group = "";

    for (index, session) in sessions.iter().enumerate() {
        let Some(group_info) = group_meta.session(index) else {
            continue;
        };
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
        let detail_tree = if !is_grouped {
            SidebarTree::None
        } else if is_last_in_group {
            SidebarTree::Blank
        } else {
            SidebarTree::Pipe
        };
        let (color, dim_color) = computed_color(
            group_index,
            group_meta.dynamic_total,
            group_info.position,
            group_total,
        );
        let selected = if selected_session.is_some() {
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
        let (display, session_indent, detail_indent) = if is_grouped {
            if group != last_group {
                items.push(SidebarItem {
                    id: SidebarItemId::Group(group),
                    display: SidebarDisplay::Text(group),
                    indent: 0,
                    tree: SidebarTree::None,
                    selectable: false,
                    session_id: None,
                    reorder_anchor: Some(reorder_anchor),
                    color,
                    dim_color,
                    kind: SidebarItemKind::Group,
                    current: false,
                });
                if max_rows.is_some_and(|limit| items.len() >= limit) {
                    break;
                }
            }
            let suffix = session_suffix(&session.name);
            let label = if suffix.is_empty() { group } else { suffix };
            let display = SidebarDisplay::Numbered {
                number: ordinal + 1,
                label,
            };
            ordinal += 1;
            (display, 2, 4)
        } else {
            let label = if group.is_empty() {
                session.name.as_str()
            } else {
                group
            };
            let display = SidebarDisplay::Numbered {
                number: ordinal + 1,
                label,
            };
            ordinal += 1;
            (display, 0, 2)
        };

        let meta = metadata.get(&session.name);
        items.push(SidebarItem {
            id: SidebarItemId::Session(session.id.as_str()),
            display,
            indent: session_indent,
            tree: session_tree,
            selectable: true,
            session_id: Some(session.id.as_str()),
            reorder_anchor: Some(reorder_anchor),
            color,
            dim_color,
            kind: SidebarItemKind::Session {
                active: selected,
                process: session.anchor.process.as_deref(),
                diff: meta.and_then(|meta| meta.diff),
            },
            current: selected,
        });
        if max_rows.is_some_and(|limit| items.len() >= limit) {
            break;
        }
        if let Some(process) = meta.and_then(|meta| meta.processes.first()) {
            items.push(SidebarItem {
                id: SidebarItemId::Process(session.id.as_str()),
                display: SidebarDisplay::Text(process.name.as_str()),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.as_str()),
                reorder_anchor: Some(reorder_anchor),
                color,
                dim_color,
                kind: SidebarItemKind::Process {
                    name: process.name.as_str(),
                    cpu_pct: Some(process.cpu_pct),
                    mem_bytes: Some(process.mem_bytes),
                },
                current: selected,
            });
            if max_rows.is_some_and(|limit| items.len() >= limit) {
                break;
            }
        } else if let Some(process) = session
            .anchor
            .process
            .as_ref()
            .filter(|process| !process.is_empty())
        {
            items.push(SidebarItem {
                id: SidebarItemId::Process(session.id.as_str()),
                display: SidebarDisplay::Text(process.as_str()),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.as_str()),
                reorder_anchor: Some(reorder_anchor),
                color,
                dim_color,
                kind: SidebarItemKind::Process {
                    name: process.as_str(),
                    cpu_pct: meta
                        .and_then(|meta| meta.process_cpu.as_deref())
                        .and_then(parse_cpu_percent),
                    mem_bytes: None,
                },
                current: selected,
            });
            if max_rows.is_some_and(|limit| items.len() >= limit) {
                break;
            }
        }
        if let Some(agent_status) = meta.and_then(|meta| meta.agent_status.as_ref()) {
            items.push(SidebarItem {
                id: SidebarItemId::Agent(session.id.as_str()),
                display: SidebarDisplay::Text(agent_status.as_str()),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.as_str()),
                reorder_anchor: Some(reorder_anchor),
                color,
                dim_color,
                kind: SidebarItemKind::Agent {
                    text: agent_status.as_str(),
                },
                current: selected,
            });
            if max_rows.is_some_and(|limit| items.len() >= limit) {
                break;
            }
        }
        if let Some(branch) = meta.and_then(|meta| meta.branch.as_ref()) {
            items.push(SidebarItem {
                id: SidebarItemId::Branch(session.id.as_str()),
                display: SidebarDisplay::Text(branch.as_str()),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.as_str()),
                reorder_anchor: Some(reorder_anchor),
                color,
                dim_color,
                kind: SidebarItemKind::Branch {
                    name: branch.as_str(),
                },
                current: selected,
            });
            if max_rows.is_some_and(|limit| items.len() >= limit) {
                break;
            }
        }
        if let Some(status) = meta.and_then(|meta| meta.status.as_ref()) {
            items.push(SidebarItem {
                id: SidebarItemId::Status(session.id.as_str()),
                display: SidebarDisplay::Text(status.as_str()),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.as_str()),
                reorder_anchor: Some(reorder_anchor),
                color,
                dim_color,
                kind: SidebarItemKind::Status {
                    text: status.as_str(),
                },
                current: selected,
            });
            if max_rows.is_some_and(|limit| items.len() >= limit) {
                break;
            }
        }
        if let Some(progress) = meta.and_then(|meta| meta.progress) {
            items.push(SidebarItem {
                id: SidebarItemId::Progress(session.id.as_str()),
                display: SidebarDisplay::Progress(progress),
                indent: detail_indent,
                tree: detail_tree,
                selectable: false,
                session_id: Some(session.id.as_str()),
                reorder_anchor: Some(reorder_anchor),
                color,
                dim_color,
                kind: SidebarItemKind::Progress { pct: progress },
                current: selected,
            });
            if max_rows.is_some_and(|limit| items.len() >= limit) {
                break;
            }
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

const HASHED_GROUP_LOOKUP_THRESHOLD: usize = 32;

impl<'a> GroupMeta<'a> {
    fn new(sessions: &'a [MuxSession]) -> Self {
        let mut groups = Vec::<GroupSummary<'a>>::new();
        let mut session_groups = Vec::with_capacity(sessions.len());
        let mut lookup = None::<HashMap<&'a str, usize>>;
        for session in sessions {
            let group = session_group(&session.name);
            if let Some(index) = lookup
                .as_ref()
                .and_then(|lookup| lookup.get(group).copied())
            {
                groups[index].count += 1;
                session_groups.push(index);
                continue;
            }
            if lookup.is_none()
                && let Some((index, existing)) = groups
                    .iter_mut()
                    .enumerate()
                    .find(|(_, summary)| summary.name == group)
            {
                existing.count += 1;
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
            if let Some(lookup) = &mut lookup {
                lookup.insert(group, index);
            } else if groups.len() > HASHED_GROUP_LOOKUP_THRESHOLD {
                lookup = Some(
                    groups
                        .iter()
                        .enumerate()
                        .map(|(index, summary)| (summary.name, index))
                        .collect(),
                );
            }
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
        assert_eq!(
            items[1].display,
            SidebarDisplay::Numbered {
                number: 1,
                label: "api"
            }
        );
        assert!(matches!(items[1].kind, SidebarItemKind::Session { .. }));
        assert!(matches!(items[2].kind, SidebarItemKind::Process { .. }));
        assert!(matches!(items[3].kind, SidebarItemKind::Branch { .. }));
        assert!(matches!(items[4].kind, SidebarItemKind::Status { .. }));
        assert!(matches!(
            items[5].kind,
            SidebarItemKind::Progress { pct: 42 }
        ));
        assert_eq!(
            items[6].display,
            SidebarDisplay::Numbered {
                number: 2,
                label: "ui"
            }
        );
        assert_eq!(items[1].tree, SidebarTree::Middle);
        assert_eq!(items[6].tree, SidebarTree::Last);
    }

    #[test]
    fn selected_session_does_not_also_mark_attached_session_current() {
        let mut sessions = vec![session("$1", "one", "zsh"), session("$2", "two", "fish")];
        sessions[0].active = true;
        let metadata = SidebarMetadata::default();

        let items = build_sidebar_items(&sessions, Some("$2"), &metadata);

        let current = items
            .iter()
            .filter(|item| matches!(item.kind, SidebarItemKind::Session { .. }) && item.current)
            .map(|item| item.session_id)
            .collect::<Vec<_>>();
        assert_eq!(current, vec![Some("$2")]);
    }

    #[test]
    fn visible_sidebar_items_match_full_prefix() {
        let sessions = vec![
            session("$1", "work/api", "zsh"),
            session("$2", "work/ui", "nvim"),
            session("$3", "work/bench", "cargo"),
            session("$4", "ops/logs", "tail"),
        ];
        let mut metadata = SidebarMetadata::default();
        metadata.insert(
            "work/api",
            SidebarSessionMetadata {
                branch: Some("main".to_owned()),
                status: Some("review".to_owned()),
                progress: Some(42),
                ..SidebarSessionMetadata::default()
            },
        );

        let full = build_sidebar_items(&sessions, Some("$2"), &metadata);
        let visible = build_visible_sidebar_items(&sessions, Some("$2"), &metadata, 5);

        assert_eq!(visible.as_slice(), &full[..5]);
    }

    #[test]
    fn visible_sidebar_items_match_full_prefix_with_many_groups() {
        let sessions = (0..40)
            .map(|index| {
                session(
                    &format!("${index}"),
                    &format!("group-{index}/session"),
                    "zsh",
                )
            })
            .collect::<Vec<_>>();
        let metadata = SidebarMetadata::default();

        let full = build_sidebar_items(&sessions, Some("$2"), &metadata);
        let visible = build_visible_sidebar_items(&sessions, Some("$2"), &metadata, 17);

        assert_eq!(visible.as_slice(), &full[..17]);
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
