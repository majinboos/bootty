use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use super::{BindingRuntime, PendingGeneratedName, WorkspaceRuntime};
use crate::ui::new_session_picker::NewMuxSessionRequest;
use bootty_mux::{RepaintHandle, command::MuxCommand, provider::GeneratedSessionNamePolicy};
use bootty_workspace::WorkspacePersistenceError;

pub(crate) enum RenameSessionOutcome {
    Missing,
    Pending,
    Started,
}

fn resolve_session_cwd(cwd: &str, remote: bool) -> String {
    if remote {
        cwd.to_owned()
    } else {
        session_root(cwd)
    }
}

fn suggested_session_name(cwd: &str, remote: bool) -> String {
    if remote {
        crate::strings::session_name_for_remote_path(cwd)
    } else {
        bootty_mux::project::suggested_session_name(cwd)
    }
}

impl BindingRuntime {
    /// Where a session lives, as bootty records it: the worktree root for a local session, and
    /// whatever the far side reported for a remote one.
    ///
    /// Memoised, because this is on the frame path and resolving a local one forks `git`.
    pub(super) fn session_cwd(&self, cwd: &str) -> String {
        if self.multiplexer.remote.is_some() {
            return resolve_session_cwd(cwd, true);
        }
        if let Some(resolved) = self.session_roots.borrow().get(cwd) {
            return resolved.clone();
        }
        let resolved = resolve_session_cwd(cwd, false);
        self.session_roots
            .borrow_mut()
            .insert(cwd.to_owned(), resolved.clone());
        resolved
    }

    pub(super) fn poll_membership_command(&mut self) {
        let Some(result) = self.mux.poll_command() else {
            return;
        };
        if result.is_err() {
            self.pending_generated_names.clear();
            self.membership_reconciliation_waiting_for_refresh = true;
            self.mux.refresh_on_next_frame();
        } else {
            self.membership_reconciliation_ready = true;
        }
    }

    pub(crate) fn clear_pending_generated_names(&mut self) {
        self.pending_generated_names.clear();
    }
}

fn session_root(cwd: &str) -> String {
    let cwd = bootty_mux::project::worktree_root(cwd).unwrap_or_else(|| cwd.to_owned());
    std::fs::canonicalize(&cwd)
        .unwrap_or_else(|_| cwd.into())
        .to_string_lossy()
        .into_owned()
}

impl WorkspaceRuntime {
    fn generated_names_signature(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for session in self.active.binding.mux.all_sessions() {
            (&session.id, &session.name, &session.anchor.cwd).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Name every claimed session after its project, and ask the backend to use that name.
    ///
    /// A shared server may add a uniqueness suffix to what it was asked for; that stays the
    /// backend's business, since what bootty shows is stored against the identity. Sessions the
    /// user renamed elsewhere are already explicit by now, and explicit names are left alone.
    pub(super) fn reconcile_generated_session_names(
        &mut self,
        repaint: &RepaintHandle,
    ) -> Result<(), WorkspacePersistenceError> {
        let remote = self.active.binding.multiplexer.remote.is_some();
        let mut candidate = self.active_reconciled_binding_state_candidate();
        let mut pending_generated_names = self.active.binding.pending_generated_names.clone();
        if self.active.binding.backend_policy.generated_session_names
            == GeneratedSessionNamePolicy::PreserveBackend
        {
            return self.commit_binding_state_candidate(candidate);
        }
        let signature = self.generated_names_signature();
        if self.active.binding.generated_names_signature == Some(signature) {
            return self.commit_binding_state_candidate(candidate);
        }

        let sessions = self.active.binding.mux.sessions().to_vec();
        // A rename bootty asked for is only still pending while the backend has not shown it.
        pending_generated_names
            .retain(|_, pending| !sessions.iter().any(|session| session.name == pending.name));
        let mut planned_names = pending_generated_names
            .values()
            .map(|pending| pending.name.clone())
            .collect::<HashSet<_>>();
        let taken_names = self.taken_session_names(None);
        let mut renames = Vec::new();

        for session in &sessions {
            let Some(identity) = session.tag.identity.as_deref() else {
                continue;
            };
            let Some(claimed) = candidate.sessions.get(identity) else {
                continue;
            };
            let cwd = session.anchor.cwd.as_deref().map_or_else(
                || claimed.cwd.clone(),
                |cwd| self.active.binding.session_cwd(cwd),
            );
            let explicit = claimed.explicit;
            let suggested = suggested_session_name(&cwd, remote);

            if explicit {
                continue;
            }
            candidate
                .sessions
                .set_display_name(identity, &suggested, false);

            let existing_names = taken_names
                .iter()
                .map(String::as_str)
                .filter(|name| *name != session.name)
                .chain(planned_names.iter().map(String::as_str));
            let desired = crate::strings::unique_session_name(&suggested, existing_names);
            // Already called what it should be, suffix and all.
            if desired == session.name
                || crate::strings::is_uniquified_session_name(&session.name, &suggested)
            {
                continue;
            }
            planned_names.insert(desired.clone());
            pending_generated_names.insert(
                session.id.clone(),
                PendingGeneratedName {
                    name: desired.clone(),
                    display_name: suggested,
                    explicit: false,
                },
            );
            renames.push((session.id.clone(), desired));
        }

        self.commit_binding_state_candidate(candidate)?;
        self.active.binding.pending_generated_names = pending_generated_names;
        self.active.binding.generated_names_signature = Some(signature);
        if renames.is_empty() {
            return Ok(());
        }
        let config = self.active.binding.multiplexer.clone();
        for (session_id, name) in renames {
            self.active
                .binding
                .mux
                .rename_session(&session_id, name, repaint, &config);
        }
        Ok(())
    }

    fn taken_session_names(&self, keep: Option<&str>) -> Vec<String> {
        self.all_bindings()
            .flat_map(|binding| {
                binding.mux.backend_session_names().iter().cloned().chain(
                    binding
                        .pending_generated_names
                        .values()
                        .map(|pending| pending.name.clone()),
                )
            })
            .filter(|name| Some(name.as_str()) != keep)
            .collect()
    }

    pub(crate) fn project_session_command(&self, cwd: &str) -> MuxCommand {
        let remote = self.active.binding.multiplexer.remote.is_some();
        let cwd = self.active.binding.session_cwd(cwd);
        let display_name = suggested_session_name(&cwd, remote);
        let session_id = crate::strings::unique_session_name(
            &display_name,
            self.taken_session_names(None).iter().map(String::as_str),
        );
        MuxCommand::CreateProjectSession {
            session_id,
            cwd,
            tag: self.active.binding.new_session_tag(),
        }
    }

    pub(crate) fn create_project_session(
        &mut self,
        command: MuxCommand,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        let remote = self.active.binding.multiplexer.remote.is_some();
        let MuxCommand::CreateProjectSession {
            session_id,
            cwd,
            tag,
        } = &command
        else {
            return Err(WorkspacePersistenceError::operation(
                "project session creation received a non-project command",
            ));
        };
        let display_name = suggested_session_name(cwd, remote);
        let pending_name = PendingGeneratedName {
            name: session_id.clone(),
            display_name,
            explicit: false,
        };
        if self
            .begin_active_binding_membership_mutation(&command, Some(&pending_name))?
            .is_none()
        {
            return Ok(false);
        }
        self.active
            .binding
            .pending_generated_names
            .insert(session_id.clone(), pending_name);
        let config = self.active.binding.multiplexer.clone();
        self.active.binding.mux.create_project_session(
            NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                tag: tag.clone(),
            },
            repaint,
            &config,
        );
        if self.active.binding.membership_completion_is_immediate() {
            self.active.binding.membership_reconciliation_ready = true;
        }
        Ok(true)
    }

    pub(crate) fn rename_active_session(
        &mut self,
        session_id: &str,
        display_name: &str,
        repaint: &RepaintHandle,
    ) -> Result<RenameSessionOutcome, WorkspacePersistenceError> {
        let Some(session) = self
            .active
            .binding
            .mux
            .session_by_id_or_name(session_id)
            .cloned()
        else {
            return Ok(RenameSessionOutcome::Missing);
        };
        let taken = self.taken_session_names(Some(session.name.as_str()));
        let backend_name =
            crate::strings::unique_session_name(display_name, taken.iter().map(String::as_str));
        let command = MuxCommand::RenameSession {
            session_id: session.id.clone(),
            name: backend_name.clone(),
        };
        let pending_name = PendingGeneratedName {
            name: backend_name.clone(),
            display_name: display_name.to_owned(),
            explicit: true,
        };
        if self
            .begin_active_binding_membership_mutation(&command, Some(&pending_name))?
            .is_none()
        {
            return Ok(RenameSessionOutcome::Pending);
        }
        self.active
            .binding
            .pending_generated_names
            .insert(session.id.clone(), pending_name);
        let config = self.active.binding.multiplexer.clone();
        self.active
            .binding
            .mux
            .rename_session(&session.id, backend_name, repaint, &config);
        if self.active.binding.membership_completion_is_immediate() {
            self.active.binding.membership_reconciliation_ready = true;
        }
        Ok(RenameSessionOutcome::Started)
    }
}
