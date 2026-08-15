use std::collections::HashSet;

use super::{BindingRuntime, WorkspaceRuntime};
use crate::ui::session_navigation::BindingSessionGroup;
use bootty_mux::snapshot::MuxSession;

const UNCLAIMED_SESSIONS_LABEL: &str = "No space";

fn session_group(
    binding: &BindingRuntime,
    label: String,
    sessions: Vec<MuxSession>,
    active: bool,
) -> BindingSessionGroup {
    BindingSessionGroup {
        scope: binding.scope,
        label,
        display_names: binding.session_display_name_map(&sessions),
        sessions,
        selected_session: binding.mux.selected_session().map(str::to_owned),
        active,
        can_return_to_last_session: binding.mux.previous_selected_session().is_some(),
    }
}

impl WorkspaceRuntime {
    pub(crate) fn active_binding_session_groups(&self) -> Vec<BindingSessionGroup> {
        let mut bindings = self.active.bindings().collect::<Vec<_>>();
        bindings.sort_by_key(|binding| binding.scope.binding_id().persistence_value());
        bindings
            .iter()
            .map(|binding| {
                let duplicate_label = bindings
                    .iter()
                    .filter(|candidate| candidate.label == binding.label)
                    .count()
                    > 1;
                let label = if duplicate_label {
                    format!(
                        "{} / Binding {}",
                        binding.label,
                        binding.scope.binding_id().persistence_value()
                    )
                } else {
                    binding.label.clone()
                };
                session_group(
                    binding,
                    label,
                    binding.mux.sessions().to_vec(),
                    binding.scope == self.active.binding.scope,
                )
            })
            .collect()
    }

    pub(crate) fn session_finder_groups(&self) -> Vec<BindingSessionGroup> {
        let mut spaces = vec![(
            self.active.position,
            self.active.name.as_str(),
            self.active.bindings().collect::<Vec<_>>(),
        )];
        spaces.extend(self.inactive_spaces.iter().map(|space| {
            (
                space.position,
                space.name.as_str(),
                space.bindings().collect::<Vec<_>>(),
            )
        }));
        spaces.sort_by_key(|(position, ..)| *position);

        let mut sessions_across_spaces = Vec::<&MuxSession>::new();
        for binding in spaces.iter().flat_map(|(_, _, bindings)| bindings) {
            for session in binding.mux.all_sessions() {
                if !sessions_across_spaces
                    .iter()
                    .any(|known| known.name == session.name)
                {
                    sessions_across_spaces.push(session);
                }
            }
        }

        let mut claimed = HashSet::new();
        let mut groups = Vec::new();
        for (_, space_name, bindings) in &spaces {
            for binding in bindings {
                let members = binding.session_order.session_names();
                let sessions = members
                    .iter()
                    .filter_map(|name| {
                        binding
                            .mux
                            .all_sessions()
                            .iter()
                            .chain(sessions_across_spaces.iter().copied())
                            .find(|session| session.name == *name)
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                claimed.extend(members);
                if sessions.is_empty() {
                    continue;
                }
                let label = if bindings.len() > 1 {
                    format!("{space_name} / {}", binding.label)
                } else {
                    (*space_name).to_owned()
                };
                groups.push(session_group(
                    binding,
                    label,
                    sessions,
                    binding.scope == self.active.binding.scope,
                ));
            }
        }

        let unclaimed = sessions_across_spaces
            .into_iter()
            .filter(|session| !claimed.contains(&session.name))
            .cloned()
            .collect::<Vec<_>>();
        if !unclaimed.is_empty() {
            let mut group = session_group(
                &self.active.binding,
                UNCLAIMED_SESSIONS_LABEL.to_owned(),
                unclaimed,
                false,
            );
            group.selected_session = None;
            group.can_return_to_last_session = false;
            group.display_names.clear();
            groups.push(group);
        }
        groups
    }
}
