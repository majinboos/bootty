#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::wildcard_imports)]

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

mod legacy;
mod schema;
mod snapshot;

use legacy::*;
use schema::*;
use snapshot::*;

use crate::sessions::{SessionMembership, WorkspaceSession};

use bootty_config::config::{MultiplexerBackendConfig, SshRemoteConfig, default_config_path};
pub use bootty_mux::membership::BackendMembership;
use bootty_mux::{
    controller::{BindingId, MuxScope, SpaceId},
    membership::MembershipOperation,
};

const WORKSPACE_SNAPSHOT_REVISION: i64 = 4;
const DEFAULT_SPACE_NAME: &str = "Default Space";
pub const DEFAULT_SPACE_ICON: &str = "folder";
pub const DEFAULT_SPACE_COLOR: [u8; 3] = [0x7A, 0xA2, 0xF7];
const DEFAULT_TINT_SIDEBAR: bool = false;
const DEFAULT_BINDING_NAME: &str = "Default Binding";

/// The one error surface for workspace persistence.
///
/// `SQLite` details stay inside this module. Callers can distinguish a persistence failure without
/// depending on rusqlite's error taxonomy or schema implementation.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("workspace persistence error: {message}")]
pub struct WorkspacePersistenceError {
    message: String,
}

impl WorkspacePersistenceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn operation(message: impl Into<String>) -> Self {
        Self::new(message)
    }
}

pub type WorkspaceResult<T> = Result<T, WorkspacePersistenceError>;

/// The membership change that Bootty asked a remote multiplexer to perform.
///
/// The row is durable until the backend result and the workspace state commit agree. The optional
/// current working directory lets recovery restore session-name metadata when it is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingMembershipMutation {
    Create {
        identity: String,
        session_name: String,
        display_name: String,
        explicit: bool,
        cwd: String,
    },
    Rename {
        identity: String,
        old_name: String,
        new_name: String,
        display_name: String,
        explicit: bool,
    },
    Ditch {
        identity: String,
        old_name: String,
    },
}

impl BindingMembershipMutation {
    pub fn identity(&self) -> &str {
        match self {
            Self::Create { identity, .. }
            | Self::Rename { identity, .. }
            | Self::Ditch { identity, .. } => identity,
        }
    }

    fn backend_operation(&self) -> MembershipOperation {
        match self {
            Self::Create {
                identity,
                session_name,
                ..
            } => MembershipOperation::Create {
                identity: identity.clone(),
                session_name: session_name.clone(),
            },
            Self::Rename {
                identity,
                old_name,
                new_name,
                ..
            } => MembershipOperation::Rename {
                identity: identity.clone(),
                old_name: old_name.clone(),
                new_name: new_name.clone(),
            },
            Self::Ditch { identity, old_name } => MembershipOperation::Ditch {
                identity: identity.clone(),
                old_name: old_name.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingBindingMembershipMutation {
    mutation: BindingMembershipMutation,
}

impl PendingBindingMembershipMutation {
    pub fn mutation(&self) -> &BindingMembershipMutation {
        &self.mutation
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceMuxOverride {
    pub backend: Option<MultiplexerBackendConfig>,
    pub remote: SpaceRemoteOverride,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSpaceRef {
    pub profile_id: String,
    pub remote_space_id: String,
    pub remote_space_name: String,
    pub backend: MultiplexerBackendConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", content = "value", rename_all = "kebab-case")]
pub enum SpaceRemoteOverride {
    #[default]
    Inherit,
    Local,
    Profile(RemoteSpaceRef),
    Inline(SshRemoteConfig),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceBinding {
    scope: MuxScope,
    name: String,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: SpaceRemoteOverride,
    hide_tmux_status: bool,
    unavailable: bool,
    selection: Option<WorkspaceBindingSelection>,
    sessions: SessionMembership,
}

impl WorkspaceBinding {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn backend_override(&self) -> Option<MultiplexerBackendConfig> {
        self.backend_override
    }

    pub fn remote_override(&self) -> &SpaceRemoteOverride {
        &self.remote_override
    }

    pub fn hide_tmux_status(&self) -> bool {
        self.hide_tmux_status
    }

    pub fn mux_scope(&self) -> MuxScope {
        self.scope
    }

    pub fn unavailable(&self) -> bool {
        self.unavailable
    }

    pub fn selection(&self) -> Option<&WorkspaceBindingSelection> {
        self.selection.as_ref()
    }

    pub fn sessions(&self) -> &SessionMembership {
        &self.sessions
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceBindingSelection {
    session_id: String,
    window_id: Option<String>,
}

impl WorkspaceBindingSelection {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn window_id(&self) -> Option<&str> {
        self.window_id.as_deref()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSpace {
    id: SpaceId,
    remote_id: String,
    name: String,
    icon: String,
    color: [u8; 3],
    tint_sidebar: bool,
    position: i64,
    bindings: Vec<WorkspaceBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    spaces: Vec<WorkspaceSpace>,
    selected_spaces: HashMap<String, SpaceId>,
    pending_binding_scopes: HashSet<MuxScope>,
}

impl WorkspaceSnapshot {
    pub fn spaces(&self) -> &[WorkspaceSpace] {
        &self.spaces
    }

    pub fn selected_space(&self, window_key: &str) -> Option<SpaceId> {
        self.selected_spaces.get(window_key).copied()
    }

    pub fn has_pending_binding_operation(&self, scope: MuxScope) -> bool {
        self.pending_binding_scopes.contains(&scope)
    }
}

impl WorkspaceSpace {
    pub fn id(&self) -> SpaceId {
        self.id
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn icon(&self) -> &str {
        &self.icon
    }

    pub fn color(&self) -> [u8; 3] {
        self.color
    }

    pub fn tint_sidebar(&self) -> bool {
        self.tint_sidebar
    }

    pub fn position(&self) -> i64 {
        self.position
    }

    pub fn bindings(&self) -> &[WorkspaceBinding] {
        &self.bindings
    }
}

#[derive(Debug)]
pub struct WorkspaceRepository {
    path: PathBuf,
}

impl WorkspaceRepository {
    pub fn open(config_path: &Path) -> WorkspaceResult<(Self, WorkspaceSnapshot)> {
        let path = sqlite_path(config_path);
        let snapshot = Self::load_or_migrate(&path)?;
        Ok((Self { path }, snapshot))
    }

    fn database_error(&self, operation: &str, error: rusqlite::Error) -> WorkspacePersistenceError {
        WorkspacePersistenceError::new(format!("{operation} at {}: {error}", self.path.display()))
    }

    pub fn create_space(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
        hide_tmux_status: bool,
    ) -> WorkspaceResult<Option<WorkspaceSpace>> {
        self.create_space_db(name, icon, color, tint_sidebar, mux, hide_tmux_status)
            .map_err(|error| self.database_error("create space", error))
    }

    fn create_space_db(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
        hide_tmux_status: bool,
    ) -> rusqlite::Result<Option<WorkspaceSpace>> {
        let name = name.trim();
        let Some(icon) = nonempty_trimmed(icon) else {
            return Ok(None);
        };
        if name.is_empty() {
            return Ok(None);
        }
        let remote = remote_to_storage(&mux.remote)?;
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction()?;
        let mut names = tx.prepare("SELECT name FROM workspace_spaces")?;
        let existing_names = names
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(names);
        let name = Self::unique_space_name(existing_names.iter().map(String::as_str), name);
        let remote_id = new_remote_space_id(&tx)?;
        let position = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM workspace_spaces",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO workspace_spaces (remote_id, name, icon, color, tint_sidebar, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                remote_id,
                name,
                icon,
                color_to_hex(color),
                i64::from(tint_sidebar),
                position
            ],
        )?;
        let space_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status, remote)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                space_id,
                DEFAULT_BINDING_NAME,
                backend_to_storage(mux.backend),
                i64::from(hide_tmux_status),
                remote,
            ],
        )?;
        let binding_id = tx.last_insert_rowid();
        tx.commit()?;

        let space = WorkspaceSpace {
            id: SpaceId::from_persistence(space_id),
            remote_id,
            name,
            icon,
            color,
            tint_sidebar,
            position,
            bindings: vec![WorkspaceBinding {
                scope: MuxScope::new(
                    SpaceId::from_persistence(space_id),
                    BindingId::from_persistence(binding_id),
                ),
                name: DEFAULT_BINDING_NAME.to_owned(),
                backend_override: mux.backend,
                remote_override: mux.remote,
                hide_tmux_status,
                unavailable: false,
                selection: None,
                sessions: SessionMembership::default(),
            }],
        };
        Ok(Some(space))
    }

    pub fn update_space_and_binding(
        &mut self,
        scope: MuxScope,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> WorkspaceResult<bool> {
        self.update_space_and_binding_db(scope, name, icon, color, tint_sidebar, mux)
            .map_err(|error| self.database_error("update space", error))
    }

    fn update_space_and_binding_db(
        &mut self,
        scope: MuxScope,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> rusqlite::Result<bool> {
        let Some(name) = nonempty_trimmed(name) else {
            return Ok(false);
        };
        let Some(icon) = nonempty_trimmed(icon) else {
            return Ok(false);
        };
        let color = color_to_hex(color);
        let backend = backend_to_storage(mux.backend);
        let remote = remote_to_storage(&mux.remote)?;
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction()?;
        if tx.execute(
            "UPDATE workspace_spaces
             SET name = ?1, icon = ?2, color = ?3, tint_sidebar = ?4
             WHERE id = ?5",
            params![
                name,
                icon,
                color,
                i64::from(tint_sidebar),
                scope.space_id().persistence_value()
            ],
        )? == 0
        {
            return Ok(false);
        }
        if tx.execute(
            "UPDATE workspace_bindings
             SET backend = ?1, remote = ?2
             WHERE id = ?3 AND space_id = ?4",
            params![
                backend,
                remote,
                scope.binding_id().persistence_value(),
                scope.space_id().persistence_value()
            ],
        )? == 0
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn delete_space(&mut self, id: SpaceId) -> WorkspaceResult<bool> {
        self.delete_space_db(id)
            .map_err(|error| self.database_error("delete space", error))
    }

    fn delete_space_db(&mut self, id: SpaceId) -> rusqlite::Result<bool> {
        let conn = open_db(&self.path)?;
        let space_count = conn.query_row("SELECT COUNT(*) FROM workspace_spaces", [], |row| {
            row.get::<_, i64>(0)
        })?;
        if space_count <= 1 {
            return Ok(false);
        }
        conn.execute(
            "DELETE FROM workspace_spaces WHERE id = ?1",
            [id.persistence_value()],
        )
        .map(|deleted| deleted != 0)
    }

    pub fn set_selected_space(
        &mut self,
        window_key: &str,
        space_id: SpaceId,
    ) -> WorkspaceResult<()> {
        self.set_selected_space_db(window_key, space_id)
            .map_err(|error| self.database_error("select space", error))
    }

    fn set_selected_space_db(&self, window_key: &str, space_id: SpaceId) -> rusqlite::Result<()> {
        let conn = open_db(&self.path)?;
        conn.execute(
            "INSERT INTO workspace_window_state (window_key, selected_space_id)
             VALUES (?1, ?2)
             ON CONFLICT(window_key) DO UPDATE SET selected_space_id = excluded.selected_space_id",
            params![window_key, space_id.persistence_value()],
        )?;
        Ok(())
    }

    pub fn set_binding_restore_state(
        &mut self,
        scope: MuxScope,
        unavailable: bool,
        session_id: Option<&str>,
        window_id: Option<&str>,
    ) -> WorkspaceResult<()> {
        let conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to save binding restore state", error)
        })?;
        let changed = conn
            .execute(
                "UPDATE workspace_bindings
             SET unavailable = ?1, selected_session_id = ?2, selected_window_id = ?3
             WHERE id = ?4 AND space_id = ?5",
                params![
                    i64::from(unavailable),
                    session_id,
                    window_id,
                    scope.binding_id().persistence_value(),
                    scope.space_id().persistence_value(),
                ],
            )
            .map_err(|error| self.database_error("save binding restore state", error))?
            != 0;
        if !changed {
            return Err(WorkspacePersistenceError::new(format!(
                "save binding restore state: binding {} does not belong to Space {}",
                scope.binding_id().persistence_value(),
                scope.space_id().persistence_value()
            )));
        }
        Ok(())
    }

    pub fn commit_binding_state(
        &mut self,
        scope: MuxScope,
        sessions: &SessionMembership,
    ) -> WorkspaceResult<()> {
        self.commit_binding_states(&[(scope, sessions.clone())])
    }

    /// Record a membership mutation before calling the backend.
    ///
    /// One row per session, so operations on different sessions never collide and nothing a user
    /// does can be refused because of a row they cannot see. A second mutation on the *same*
    /// session supersedes the first, which is what reconciliation would do with one whose effect
    /// it cannot observe anyway.
    pub fn begin_binding_membership_mutation(
        &mut self,
        scope: MuxScope,
        mutation: &BindingMembershipMutation,
    ) -> WorkspaceResult<()> {
        validate_binding_membership_mutation(mutation)?;
        let mut conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to journal binding membership", error)
        })?;
        let tx = conn
            .transaction()
            .map_err(|error| self.database_error("begin binding membership journal", error))?;
        self.validate_binding_scope(&tx, scope)?;
        let stored = binding_membership_mutation_to_storage(mutation);
        tx.execute(
            "INSERT INTO workspace_pending_binding_operations
                (space_id, binding_id, operation, identity, old_name, new_name,
                 display_name, explicit, cwd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(identity) DO UPDATE SET
                space_id = excluded.space_id,
                binding_id = excluded.binding_id,
                operation = excluded.operation,
                old_name = excluded.old_name,
                new_name = excluded.new_name,
                display_name = excluded.display_name,
                explicit = excluded.explicit,
                cwd = excluded.cwd",
            params![
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value(),
                stored.operation,
                stored.identity,
                stored.old_name,
                stored.new_name,
                stored.display_name,
                stored.explicit,
                stored.cwd,
            ],
        )
        .map_err(|error| self.database_error("journal binding membership", error))?;
        tx.commit()
            .map_err(|error| self.database_error("commit binding membership journal", error))?;
        Ok(())
    }

    pub fn pending_binding_membership_mutations(
        &mut self,
        scope: MuxScope,
    ) -> WorkspaceResult<Vec<PendingBindingMembershipMutation>> {
        let conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to read binding membership", error)
        })?;
        self.load_pending_binding_membership_mutations(&conn, scope)
    }

    /// Apply a completed remote mutation and clear its journal row in one transaction.
    ///
    /// The in-memory stores publish only after `SQLite` commits. A failure therefore leaves both the
    /// old stores and the pending intent available for the next remote catalog operation.
    pub fn commit_binding_membership_mutation(
        &mut self,
        scope: MuxScope,
        mutation: &BindingMembershipMutation,
        sessions: &mut SessionMembership,
    ) -> WorkspaceResult<()> {
        validate_binding_membership_mutation(mutation)?;
        let mut next = sessions.clone();
        apply_binding_membership_mutation(mutation, &mut next)?;

        let mut conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to commit binding membership", error)
        })?;
        let tx = conn
            .transaction()
            .map_err(|error| self.database_error("begin binding membership commit", error))?;
        self.require_pending_binding_membership_mutation(&tx, scope, mutation)?;
        self.write_binding_state(&tx, scope, &next)?;
        self.delete_pending_binding_membership_mutation(&tx, mutation.identity())?;
        tx.commit()
            .map_err(|error| self.database_error("commit binding membership", error))?;
        *sessions = next;
        Ok(())
    }

    /// Settle every leftover mutation for a binding against a fresh backend snapshot.
    ///
    /// Each one is applied when the snapshot shows a session carrying its identity, and discarded
    /// when it does not. Either way the row goes.
    pub fn reconcile_binding_membership_mutations(
        &mut self,
        scope: MuxScope,
        memberships: &[BackendMembership],
        sessions: &mut SessionMembership,
    ) -> WorkspaceResult<bool> {
        let mut conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to reconcile binding membership", error)
        })?;
        let tx = conn.transaction().map_err(|error| {
            self.database_error("begin binding membership reconciliation", error)
        })?;
        let pending = self.load_pending_binding_membership_mutations(&tx, scope)?;
        if pending.is_empty() {
            return Ok(false);
        }
        let mut next = sessions.clone();
        for pending in &pending {
            if pending
                .mutation
                .backend_operation()
                .effect_occurred(memberships)
            {
                apply_binding_membership_mutation(&pending.mutation, &mut next)?;
            }
            self.delete_pending_binding_membership_mutation(&tx, pending.mutation.identity())?;
        }
        self.write_binding_state(&tx, scope, &next)?;
        tx.commit().map_err(|error| {
            self.database_error("commit binding membership reconciliation", error)
        })?;
        *sessions = next;
        Ok(true)
    }

    fn validate_binding_scope(&self, tx: &Transaction<'_>, scope: MuxScope) -> WorkspaceResult<()> {
        let exists = binding_scope_exists(tx, scope)
            .map_err(|error| self.database_error("validate binding membership scope", error))?;
        if !exists {
            return Err(WorkspacePersistenceError::new(format!(
                "binding membership scope: binding {} does not belong to Space {}",
                scope.binding_id().persistence_value(),
                scope.space_id().persistence_value()
            )));
        }
        Ok(())
    }

    fn load_pending_binding_membership_mutations(
        &self,
        conn: &Connection,
        scope: MuxScope,
    ) -> WorkspaceResult<Vec<PendingBindingMembershipMutation>> {
        let load = || -> rusqlite::Result<Vec<PendingBindingMembershipMutation>> {
            let mut statement = conn.prepare(
                "SELECT operation, identity, old_name, new_name, display_name, explicit, cwd
                 FROM workspace_pending_binding_operations
                 WHERE space_id = ?1 AND binding_id = ?2
                 ORDER BY identity",
            )?;
            let rows = statement.query_map(
                params![
                    scope.space_id().persistence_value(),
                    scope.binding_id().persistence_value()
                ],
                |row| {
                    binding_membership_mutation_from_row(row, 0)
                        .map(|mutation| PendingBindingMembershipMutation { mutation })
                },
            )?;
            rows.collect()
        };
        load().map_err(|error| self.database_error("load binding membership journal", error))
    }

    fn require_pending_binding_membership_mutation(
        &self,
        tx: &Transaction<'_>,
        scope: MuxScope,
        mutation: &BindingMembershipMutation,
    ) -> WorkspaceResult<()> {
        let pending = self.load_pending_binding_membership_mutations(tx, scope)?;
        if !pending.iter().any(|pending| pending.mutation == *mutation) {
            return Err(WorkspacePersistenceError::new(
                "commit binding membership: pending mutation is missing or superseded",
            ));
        }
        Ok(())
    }

    fn delete_pending_binding_membership_mutation(
        &self,
        tx: &Transaction<'_>,
        identity: &str,
    ) -> WorkspaceResult<()> {
        tx.execute(
            "DELETE FROM workspace_pending_binding_operations WHERE identity = ?1",
            [identity],
        )
        .map_err(|error| self.database_error("delete binding membership journal", error))?;
        Ok(())
    }

    /// Commit several binding candidates as one durable workspace mutation.
    ///
    /// The transaction is all-or-nothing. Callers can publish the candidates only after this
    /// method succeeds.
    pub fn commit_binding_states(
        &mut self,
        states: &[(MuxScope, SessionMembership)],
    ) -> WorkspaceResult<()> {
        if states.is_empty() {
            return Ok(());
        }
        Self::validate_binding_states(states)?;
        let mut conn = open_db(&self.path)
            .map_err(|error| self.database_error("open database to commit binding state", error))?;
        let tx = conn
            .transaction()
            .map_err(|error| self.database_error("begin binding state commit", error))?;
        for (scope, sessions) in states {
            self.write_binding_state(&tx, *scope, sessions)?;
        }
        tx.commit()
            .map_err(|error| self.database_error("commit binding state", error))?;

        Ok(())
    }

    fn validate_binding_states(states: &[(MuxScope, SessionMembership)]) -> WorkspaceResult<()> {
        let mut scopes = HashSet::new();
        for (scope, sessions) in states {
            if !scopes.insert(*scope) {
                return Err(WorkspacePersistenceError::new(format!(
                    "validate binding state: duplicate binding {} in Space {}",
                    scope.binding_id().persistence_value(),
                    scope.space_id().persistence_value()
                )));
            }

            let mut identities = HashSet::new();
            for session in sessions.sessions() {
                // Only the identity has to be unique. Two sessions may legitimately share a
                // display name, and a shared server may have made their backend names differ by a
                // suffix bootty does not show.
                if session.identity.is_empty()
                    || session.identity.contains('\0')
                    || session.backend_name.is_empty()
                    || session.backend_name.contains('\0')
                    || session.display_name.contains('\0')
                    || session.cwd.contains('\0')
                    || !identities.insert(session.identity.as_str())
                {
                    return Err(WorkspacePersistenceError::new(
                        "validate binding state: session membership is invalid or duplicated",
                    ));
                }
            }
        }
        Ok(())
    }

    fn write_binding_state(
        &self,
        tx: &Transaction<'_>,
        scope: MuxScope,
        sessions: &SessionMembership,
    ) -> WorkspaceResult<()> {
        let binding_exists = binding_scope_exists(tx, scope)
            .map_err(|error| self.database_error("validate binding state scope", error))?;
        if !binding_exists {
            return Err(WorkspacePersistenceError::new(format!(
                "commit binding state: binding {} does not belong to Space {}",
                scope.binding_id().persistence_value(),
                scope.space_id().persistence_value()
            )));
        }

        let binding_id = scope.binding_id().persistence_value();
        tx.execute(
            "DELETE FROM workspace_sessions WHERE binding_id = ?1",
            [binding_id],
        )
        .map_err(|error| self.database_error("replace persisted sessions", error))?;
        for (position, session) in sessions.sessions().iter().enumerate() {
            tx.execute(
                "INSERT INTO workspace_sessions
                    (identity, binding_id, backend_name, display_name, explicit, cwd, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    session.identity,
                    binding_id,
                    session.backend_name,
                    session.display_name,
                    i64::from(session.explicit),
                    session.cwd,
                    position as i64
                ],
            )
            .map_err(|error| self.database_error("insert persisted session", error))?;
        }
        Ok(())
    }

    fn unique_space_name<'a>(
        existing: impl IntoIterator<Item = &'a str>,
        requested: &str,
    ) -> String {
        let existing = existing
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect::<HashSet<_>>();
        if !existing.contains(&requested.to_ascii_lowercase()) {
            return requested.to_owned();
        }
        for suffix in 2.. {
            let candidate = format!("{requested} {suffix}");
            if !existing.contains(&candidate.to_ascii_lowercase()) {
                return candidate;
            }
        }
        unreachable!("unbounded integer suffixes always produce a unique space name")
    }

    fn load_or_migrate(path: &Path) -> WorkspaceResult<WorkspaceSnapshot> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                WorkspacePersistenceError::new(format!(
                    "create database directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let result = (|| {
            let mut conn = open_db(path)?;
            let revision: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if revision > WORKSPACE_SNAPSHOT_REVISION {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let schema = classify_schema(&conn, revision)?;
            if schema.uses_legacy_binding_cardinality() {
                migrate_workspace_binding_cardinality(&conn)?;
            }
            let tx = conn.transaction()?;
            // Before the current schema is created, so revision 3's tables are still there to read.
            migrate_workspace_sessions_to_identities(&tx)?;
            create_workspace_schema(&tx)?;
            migrate_workspace_space_icons(&tx)?;
            migrate_workspace_remote_ids(&tx)?;
            migrate_workspace_space_appearance(&tx)?;
            migrate_workspace_snapshot_state(&tx)?;
            let space_count = tx.query_row("SELECT COUNT(*) FROM workspace_spaces", [], |row| {
                row.get::<_, i64>(0)
            })?;
            if space_count == 0 {
                if !schema.allows_default_creation() {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                create_default_binding(&tx, path)?;
            }
            let spaces = load_spaces(&tx)?;
            let pending_binding_scopes = validate_pending_binding_operations(&tx, &spaces)?;
            let space_ids = spaces
                .iter()
                .map(|space| space.id.persistence_value())
                .collect::<HashSet<_>>();
            let mut selected_spaces = HashMap::new();
            let mut statement = tx.prepare(
                "SELECT window_key, selected_space_id FROM workspace_window_state ORDER BY window_key",
            )?;
            for row in statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (window_key, selected_space_id) = row?;
                if window_key.trim().is_empty()
                    || !space_ids.contains(&selected_space_id)
                    || selected_spaces
                        .insert(window_key, SpaceId::from_persistence(selected_space_id))
                        .is_some()
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
            }
            drop(statement);
            tx.pragma_update(None, "user_version", WORKSPACE_SNAPSHOT_REVISION)?;
            tx.commit()?;
            Ok(WorkspaceSnapshot {
                spaces,
                selected_spaces,
                pending_binding_scopes,
            })
        })();
        result.map_err(|error| {
            WorkspacePersistenceError::new(format!("load or migrate {}: {error}", path.display()))
        })
    }
}

fn validate_pending_binding_operations(
    tx: &Transaction<'_>,
    spaces: &[WorkspaceSpace],
) -> rusqlite::Result<HashSet<MuxScope>> {
    let scopes = spaces
        .iter()
        .flat_map(|space| space.bindings.iter().map(|binding| binding.scope))
        .collect::<HashSet<_>>();
    let mut statement = tx.prepare(
        "SELECT space_id, binding_id, operation, identity, old_name, new_name,
                display_name, explicit, cwd
         FROM workspace_pending_binding_operations ORDER BY space_id, binding_id",
    )?;
    let mut pending_scopes = HashSet::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            binding_membership_mutation_from_row(row, 2)?,
        ))
    })? {
        let (space_id, binding_id, _mutation) = row?;
        if space_id <= 0 || binding_id <= 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let scope = MuxScope::new(
            SpaceId::from_persistence(space_id),
            BindingId::from_persistence(binding_id),
        );
        if !scopes.contains(&scope) || !pending_scopes.insert(scope) {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    Ok(pending_scopes)
}

fn validate_binding_membership_mutation(
    mutation: &BindingMembershipMutation,
) -> WorkspaceResult<()> {
    mutation
        .backend_operation()
        .validate()
        .map_err(|_| WorkspacePersistenceError::new("binding membership mutation is invalid"))?;
    let valid_text = |value: &str| !value.is_empty() && !value.contains('\0');
    let valid = match mutation {
        BindingMembershipMutation::Create {
            display_name, cwd, ..
        } => valid_text(display_name) && !cwd.contains('\0'),
        BindingMembershipMutation::Rename { display_name, .. } => valid_text(display_name),
        BindingMembershipMutation::Ditch { .. } => true,
    };
    valid
        .then_some(())
        .ok_or_else(|| WorkspacePersistenceError::new("binding membership mutation is invalid"))
}

struct StoredBindingMembershipMutation<'a> {
    operation: &'static str,
    identity: &'a str,
    old_name: Option<&'a str>,
    new_name: Option<&'a str>,
    display_name: Option<&'a str>,
    explicit: Option<bool>,
    cwd: Option<&'a str>,
}

fn binding_membership_mutation_to_storage(
    mutation: &BindingMembershipMutation,
) -> StoredBindingMembershipMutation<'_> {
    match mutation {
        BindingMembershipMutation::Create {
            identity,
            session_name,
            display_name,
            explicit,
            cwd,
        } => StoredBindingMembershipMutation {
            operation: "create",
            identity,
            old_name: None,
            new_name: Some(session_name),
            display_name: Some(display_name),
            explicit: Some(*explicit),
            cwd: Some(cwd),
        },
        BindingMembershipMutation::Rename {
            identity,
            old_name,
            new_name,
            display_name,
            explicit,
        } => StoredBindingMembershipMutation {
            operation: "rename",
            identity,
            old_name: Some(old_name),
            new_name: Some(new_name),
            display_name: Some(display_name),
            explicit: Some(*explicit),
            cwd: None,
        },
        BindingMembershipMutation::Ditch { identity, old_name } => {
            StoredBindingMembershipMutation {
                operation: "ditch",
                identity,
                old_name: Some(old_name),
                new_name: None,
                display_name: None,
                explicit: None,
                cwd: None,
            }
        }
    }
}

fn binding_membership_mutation_from_storage(
    operation: &str,
    identity: String,
    old_name: Option<String>,
    new_name: Option<String>,
    display_name: Option<String>,
    explicit: Option<i64>,
    cwd: Option<String>,
) -> WorkspaceResult<BindingMembershipMutation> {
    let explicit = explicit
        .map(|value| {
            bool_from_storage(value).map_err(|_| {
                WorkspacePersistenceError::new("binding membership explicit-name state is invalid")
            })
        })
        .transpose()?;
    let missing = |what: &str| WorkspacePersistenceError::new(format!("binding membership {what}"));
    let mutation = match operation {
        "create" if old_name.is_none() => BindingMembershipMutation::Create {
            identity,
            session_name: new_name.ok_or_else(|| missing("create is missing its name"))?,
            display_name: display_name
                .ok_or_else(|| missing("create is missing its display name"))?,
            explicit: explicit
                .ok_or_else(|| missing("create is missing its explicit-name state"))?,
            cwd: cwd.unwrap_or_default(),
        },
        "rename" if cwd.is_none() => BindingMembershipMutation::Rename {
            identity,
            old_name: old_name.ok_or_else(|| missing("rename is missing its old name"))?,
            new_name: new_name.ok_or_else(|| missing("rename is missing its new name"))?,
            display_name: display_name
                .ok_or_else(|| missing("rename is missing its display name"))?,
            explicit: explicit
                .ok_or_else(|| missing("rename is missing its explicit-name state"))?,
        },
        "ditch"
            if new_name.is_none()
                && display_name.is_none()
                && explicit.is_none()
                && cwd.is_none() =>
        {
            BindingMembershipMutation::Ditch {
                identity,
                old_name: old_name.ok_or_else(|| missing("ditch is missing its old name"))?,
            }
        }
        _ => {
            return Err(WorkspacePersistenceError::new(
                "binding membership operation is unknown",
            ));
        }
    };
    validate_binding_membership_mutation(&mutation)?;
    Ok(mutation)
}

fn binding_membership_mutation_from_row(
    row: &Row<'_>,
    offset: usize,
) -> rusqlite::Result<BindingMembershipMutation> {
    let operation = row.get::<_, String>(offset)?;
    binding_membership_mutation_from_storage(
        &operation,
        row.get(offset + 1)?,
        row.get(offset + 2)?,
        row.get(offset + 3)?,
        row.get(offset + 4)?,
        row.get(offset + 5)?,
        row.get(offset + 6)?,
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn binding_scope_exists(tx: &Transaction<'_>, scope: MuxScope) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM workspace_bindings
             WHERE id = ?1 AND space_id = ?2
         )",
        params![
            scope.binding_id().persistence_value(),
            scope.space_id().persistence_value()
        ],
        |row| row.get(0),
    )
}

/// Apply a mutation whose backend effect is known to have happened. Keyed on the identity, so it
/// lands on the session it was issued for even if the name moved on in between.
fn apply_binding_membership_mutation(
    mutation: &BindingMembershipMutation,
    sessions: &mut SessionMembership,
) -> WorkspaceResult<()> {
    match mutation {
        BindingMembershipMutation::Create {
            identity,
            session_name,
            display_name,
            explicit,
            cwd,
        } => {
            // A create that already landed is not an error: reconciliation replays the same
            // mutation the commit path did, and both have to agree on the outcome.
            sessions.claim(WorkspaceSession {
                identity: identity.clone(),
                backend_name: session_name.clone(),
                display_name: display_name.clone(),
                explicit: *explicit,
                cwd: cwd.clone(),
            });
            sessions.observe_backend_name(identity, session_name);
            sessions.set_display_name(identity, display_name, *explicit);
        }
        BindingMembershipMutation::Rename {
            identity,
            old_name: _,
            new_name,
            display_name,
            explicit,
        } => {
            if !sessions.contains(identity) {
                return Err(WorkspacePersistenceError::new(
                    "apply binding membership: renamed session is not claimed by this Space",
                ));
            }
            sessions.observe_backend_name(identity, new_name);
            sessions.set_display_name(identity, display_name, *explicit);
        }
        BindingMembershipMutation::Ditch { identity, .. } => {
            // The session is gone for good, so its name goes with it. Leaving the record behind is
            // what used to make the next session started in the same directory inherit a dead
            // session's name.
            sessions.release(identity);
        }
    }
    Ok(())
}

pub(crate) fn sqlite_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("session-order.sqlite3")
}

pub(crate) fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(250))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

fn create_default_binding(tx: &Transaction<'_>, path: &Path) -> rusqlite::Result<WorkspaceBinding> {
    let remote_id = new_remote_space_id(tx)?;
    tx.execute(
        "INSERT INTO workspace_spaces (remote_id, name, icon, color, tint_sidebar, position)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![
            remote_id,
            DEFAULT_SPACE_NAME,
            DEFAULT_SPACE_ICON,
            color_to_hex(DEFAULT_SPACE_COLOR),
            i64::from(DEFAULT_TINT_SIDEBAR)
        ],
    )?;
    let space_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            space_id,
            DEFAULT_BINDING_NAME,
            backend_to_storage(None),
            0_i64,
        ],
    )?;
    let binding_id = tx.last_insert_rowid();
    migrate_legacy_metadata(tx, binding_id, path)?;
    Ok(WorkspaceBinding {
        scope: MuxScope::new(
            SpaceId::from_persistence(space_id),
            BindingId::from_persistence(binding_id),
        ),
        name: DEFAULT_BINDING_NAME.to_owned(),
        backend_override: None,
        remote_override: SpaceRemoteOverride::Inherit,
        hide_tmux_status: false,
        unavailable: false,
        selection: None,
        sessions: SessionMembership::default(),
    })
}
