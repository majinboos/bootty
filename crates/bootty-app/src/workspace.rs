use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub use crate::{
    session_names::{SessionNameRecord, SessionNameStore},
    session_order::SessionOrderStore,
};

use crate::{
    config::{MultiplexerBackendConfig, MultiplexerConfig, SshRemoteConfig, default_config_path},
    mux::{
        controller::{BindingId, MuxScope, SpaceId},
        membership::{BackendMembership, MembershipOperation},
    },
    session_order::SessionGroup,
};

const WORKSPACE_SNAPSHOT_REVISION: i64 = 3;
const DEFAULT_SPACE_NAME: &str = "Default Space";
pub const DEFAULT_SPACE_ICON: &str = "folder";
pub const DEFAULT_SPACE_COLOR: [u8; 3] = [0x7A, 0xA2, 0xF7];
const DEFAULT_TINT_SIDEBAR: bool = false;
const DEFAULT_BINDING_NAME: &str = "Default Binding";

/// The one error surface for workspace persistence.
///
/// SQLite details stay inside this module. Callers can distinguish a persistence failure without
/// depending on rusqlite's error taxonomy or schema implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePersistenceError {
    message: String,
}

impl WorkspacePersistenceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn operation(message: impl Into<String>) -> Self {
        Self::new(message)
    }
}

impl fmt::Display for WorkspacePersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "workspace persistence error: {}", self.message)
    }
}

impl std::error::Error for WorkspacePersistenceError {}

pub type WorkspaceResult<T> = Result<T, WorkspacePersistenceError>;

/// The membership change that Bootty asked a remote multiplexer to perform.
///
/// The row is durable until the backend result and the workspace state commit agree. The optional
/// current working directory lets recovery restore session-name metadata when it is available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingMembershipMutation {
    Create {
        session_id: String,
        session_name: String,
        display_name: String,
        explicit: bool,
        cwd: Option<String>,
    },
    Rename {
        session_id: String,
        old_name: String,
        new_name: String,
        display_name: String,
        explicit: bool,
        cwd: Option<String>,
    },
    Ditch {
        session_id: String,
        old_name: String,
    },
}

impl BindingMembershipMutation {
    fn backend_operation(&self) -> MembershipOperation {
        match self {
            Self::Create {
                session_id,
                session_name,
                ..
            } => MembershipOperation::Create {
                session_id: session_id.clone(),
                session_name: session_name.clone(),
            },
            Self::Rename {
                session_id,
                old_name,
                new_name,
                ..
            } => MembershipOperation::Rename {
                session_id: session_id.clone(),
                old_name: old_name.clone(),
                new_name: new_name.clone(),
            },
            Self::Ditch {
                session_id,
                old_name,
            } => MembershipOperation::Ditch {
                session_id: session_id.clone(),
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

pub type BackendSessionMembership = BackendMembership;

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
    session_order: SessionOrderStore,
    session_names: SessionNameStore,
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

    pub fn session_order(&self) -> &SessionOrderStore {
        &self.session_order
    }

    pub fn session_names(&self) -> &SessionNameStore {
        &self.session_names
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

    pub(crate) fn has_pending_binding_operation(&self, scope: MuxScope) -> bool {
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
        config: &MultiplexerConfig,
    ) -> WorkspaceResult<Option<WorkspaceSpace>> {
        self.create_space_db(name, icon, color, tint_sidebar, mux, config)
            .map_err(|error| self.database_error("create space", error))
    }

    fn create_space_db(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
        config: &MultiplexerConfig,
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
                i64::from(config.hide_tmux_status),
                remote,
            ],
        )?;
        let binding_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, '', 0)",
            [binding_id],
        )?;
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
                hide_tmux_status: config.hide_tmux_status,
                unavailable: false,
                selection: None,
                session_order: SessionOrderStore::from_groups(
                    vec![SessionGroup {
                        name: String::new(),
                        sessions: Vec::new(),
                    }],
                    true,
                ),
                session_names: SessionNameStore::default(),
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

    pub(crate) fn delete_space(&mut self, id: SpaceId) -> WorkspaceResult<bool> {
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
        if conn.execute(
            "DELETE FROM workspace_spaces WHERE id = ?1",
            [id.persistence_value()],
        )? == 0
        {
            return Ok(false);
        }
        Ok(true)
    }

    pub(crate) fn set_selected_space(
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

    pub(crate) fn set_binding_restore_state(
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
        session_order: &SessionOrderStore,
        session_names: &SessionNameStore,
    ) -> WorkspaceResult<()> {
        let states = [(scope, session_order.clone(), session_names.clone())];
        self.commit_binding_states(&states)
    }

    /// Record a binding membership mutation before calling the backend.
    ///
    /// One binding can have one unresolved remote mutation. The unique scope protects the journal
    /// from overlapping commands that could not be reconciled in order.
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
                (space_id, binding_id, operation, session_id, old_name, new_name,
                 display_name, explicit, cwd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value(),
                stored.operation,
                stored.session_id,
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

    pub fn pending_binding_membership_mutation(
        &mut self,
        scope: MuxScope,
    ) -> WorkspaceResult<Option<PendingBindingMembershipMutation>> {
        let mut conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to read binding membership", error)
        })?;
        let tx = conn
            .transaction()
            .map_err(|error| self.database_error("begin binding membership read", error))?;
        let pending = self.load_pending_binding_membership_mutation(&tx, scope)?;
        tx.commit()
            .map_err(|error| self.database_error("commit binding membership read", error))?;
        Ok(pending)
    }

    /// Apply a completed remote mutation and clear its journal row in one transaction.
    ///
    /// The in-memory stores publish only after SQLite commits. A failure therefore leaves both the
    /// old stores and the pending intent available for the next remote catalog operation.
    pub fn commit_binding_membership_mutation(
        &mut self,
        scope: MuxScope,
        mutation: &BindingMembershipMutation,
        session_order: &mut SessionOrderStore,
        session_names: &mut SessionNameStore,
    ) -> WorkspaceResult<()> {
        validate_binding_membership_mutation(mutation)?;
        let mut next_order = session_order.clone();
        let mut next_names = session_names.clone();
        apply_binding_membership_mutation(mutation, &mut next_order, &mut next_names)?;

        let mut conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to commit binding membership", error)
        })?;
        let tx = conn
            .transaction()
            .map_err(|error| self.database_error("begin binding membership commit", error))?;
        self.require_pending_binding_membership_mutation(&tx, scope, mutation)?;
        self.write_binding_state(&tx, scope, &next_order, &next_names)?;
        self.delete_pending_binding_membership_mutation(&tx, scope)?;
        tx.commit()
            .map_err(|error| self.database_error("commit binding membership", error))?;
        *session_order = next_order;
        *session_names = next_names;
        Ok(())
    }

    /// Reconcile a leftover remote mutation against a fresh backend membership snapshot.
    ///
    /// The repository applies the mutation when the snapshot proves that the backend effect
    /// happened. It only clears the row when the snapshot proves that the effect did not happen.
    pub fn reconcile_binding_membership_mutation(
        &mut self,
        scope: MuxScope,
        memberships: &[BackendSessionMembership],
        session_order: &mut SessionOrderStore,
        session_names: &mut SessionNameStore,
    ) -> WorkspaceResult<bool> {
        let mut conn = open_db(&self.path).map_err(|error| {
            self.database_error("open database to reconcile binding membership", error)
        })?;
        let tx = conn.transaction().map_err(|error| {
            self.database_error("begin binding membership reconciliation", error)
        })?;
        let Some(pending) = self.load_pending_binding_membership_mutation(&tx, scope)? else {
            return Ok(false);
        };
        if pending
            .mutation
            .backend_operation()
            .effect_occurred(memberships)
        {
            let mut next_order = session_order.clone();
            let mut next_names = session_names.clone();
            apply_binding_membership_mutation(&pending.mutation, &mut next_order, &mut next_names)?;
            self.write_binding_state(&tx, scope, &next_order, &next_names)?;
            self.delete_pending_binding_membership_mutation(&tx, scope)?;
            tx.commit().map_err(|error| {
                self.database_error("commit binding membership reconciliation", error)
            })?;
            *session_order = next_order;
            *session_names = next_names;
            Ok(true)
        } else {
            self.delete_pending_binding_membership_mutation(&tx, scope)?;
            tx.commit().map_err(|error| {
                self.database_error("discard binding membership reconciliation", error)
            })?;
            Ok(true)
        }
    }

    fn validate_binding_scope(&self, tx: &Transaction<'_>, scope: MuxScope) -> WorkspaceResult<()> {
        let exists = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM workspace_bindings
                     WHERE id = ?1 AND space_id = ?2
                 )",
                params![
                    scope.binding_id().persistence_value(),
                    scope.space_id().persistence_value()
                ],
                |row| row.get::<_, bool>(0),
            )
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

    fn load_pending_binding_membership_mutation(
        &self,
        tx: &Transaction<'_>,
        scope: MuxScope,
    ) -> WorkspaceResult<Option<PendingBindingMembershipMutation>> {
        tx.query_row(
            "SELECT operation, session_id, old_name, new_name, display_name, explicit, cwd
             FROM workspace_pending_binding_operations
             WHERE space_id = ?1 AND binding_id = ?2",
            params![
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value()
            ],
            |row| {
                let operation = row.get::<_, String>(0)?;
                let session_id = row.get::<_, String>(1)?;
                let old_name = row.get::<_, Option<String>>(2)?;
                let new_name = row.get::<_, Option<String>>(3)?;
                let display_name = row.get::<_, Option<String>>(4)?;
                let explicit = row.get::<_, Option<i64>>(5)?;
                let cwd = row.get::<_, Option<String>>(6)?;
                binding_membership_mutation_from_storage(
                    &operation,
                    session_id,
                    old_name,
                    new_name,
                    display_name,
                    explicit,
                    cwd,
                )
                .map(|mutation| PendingBindingMembershipMutation { mutation })
                .map_err(|_| rusqlite::Error::InvalidQuery)
            },
        )
        .optional()
        .map_err(|error| self.database_error("load binding membership journal", error))
    }

    fn require_pending_binding_membership_mutation(
        &self,
        tx: &Transaction<'_>,
        scope: MuxScope,
        mutation: &BindingMembershipMutation,
    ) -> WorkspaceResult<()> {
        let Some(pending) = self.load_pending_binding_membership_mutation(tx, scope)? else {
            return Err(WorkspacePersistenceError::new(
                "commit binding membership: pending mutation is missing",
            ));
        };
        if pending.mutation != *mutation {
            return Err(WorkspacePersistenceError::new(
                "commit binding membership: pending mutation does not match",
            ));
        }
        Ok(())
    }

    fn delete_pending_binding_membership_mutation(
        &self,
        tx: &Transaction<'_>,
        scope: MuxScope,
    ) -> WorkspaceResult<()> {
        tx.execute(
            "DELETE FROM workspace_pending_binding_operations
             WHERE space_id = ?1 AND binding_id = ?2",
            params![
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value()
            ],
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
        states: &[(MuxScope, SessionOrderStore, SessionNameStore)],
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
        for (scope, session_order, session_names) in states {
            self.write_binding_state(&tx, *scope, session_order, session_names)?;
        }
        tx.commit()
            .map_err(|error| self.database_error("commit binding state", error))?;

        Ok(())
    }

    fn validate_binding_states(
        states: &[(MuxScope, SessionOrderStore, SessionNameStore)],
    ) -> WorkspaceResult<()> {
        let mut scopes = HashSet::new();
        for (scope, session_order, session_names) in states {
            if !scopes.insert(*scope) {
                return Err(WorkspacePersistenceError::new(format!(
                    "validate binding state: duplicate binding {} in Space {}",
                    scope.binding_id().persistence_value(),
                    scope.space_id().persistence_value()
                )));
            }

            let mut sessions = HashSet::new();
            for group in session_order.groups() {
                if group.name.contains('\0') {
                    return Err(WorkspacePersistenceError::new(
                        "validate binding state: group name contains a null byte",
                    ));
                }
                for session in &group.sessions {
                    if session.is_empty()
                        || session.contains('\0')
                        || !sessions.insert(session.as_str())
                    {
                        return Err(WorkspacePersistenceError::new(
                            "validate binding state: session membership is invalid or duplicated",
                        ));
                    }
                }
            }

            let mut directories = HashSet::new();
            for (key, record) in session_names.records() {
                if key != &record.session_id
                    || record.session_id.is_empty()
                    || record.session_id.contains('\0')
                    || record.cwd.contains('\0')
                    || record.generated_name.is_empty()
                    || record.generated_name.contains('\0')
                    || record.session_name.contains('\0')
                    || record.display_name.contains('\0')
                    || (!record.cwd.is_empty() && !directories.insert(record.cwd.as_str()))
                {
                    return Err(WorkspacePersistenceError::new(
                        "validate binding state: session name metadata is invalid or duplicated",
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
        session_order: &SessionOrderStore,
        session_names: &SessionNameStore,
    ) -> WorkspaceResult<()> {
        let binding_exists = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM workspace_bindings
                     WHERE id = ?1 AND space_id = ?2
                 )",
                params![
                    scope.binding_id().persistence_value(),
                    scope.space_id().persistence_value()
                ],
                |row| row.get::<_, bool>(0),
            )
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
        .map_err(|error| self.database_error("replace persisted session order", error))?;
        tx.execute(
            "DELETE FROM workspace_session_groups WHERE binding_id = ?1",
            [binding_id],
        )
        .map_err(|error| self.database_error("replace persisted session groups", error))?;
        for (group_position, group) in session_order.groups().iter().enumerate() {
            tx.execute(
                "INSERT INTO workspace_session_groups (binding_id, name, position)
                 VALUES (?1, ?2, ?3)",
                params![binding_id, group.name, group_position as i64],
            )
            .map_err(|error| self.database_error("insert persisted session group", error))?;
            let group_id = tx.last_insert_rowid();
            for (session_position, session) in group.sessions.iter().enumerate() {
                tx.execute(
                    "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![binding_id, session, group_id, session_position as i64],
                )
                .map_err(|error| self.database_error("insert persisted session order", error))?;
            }
        }
        if session_order.groups().is_empty() {
            tx.execute(
                "INSERT INTO workspace_session_groups (binding_id, name, position)
                 VALUES (?1, '', 0)",
                [binding_id],
            )
            .map_err(|error| self.database_error("initialize persisted session group", error))?;
        }

        tx.execute(
            "DELETE FROM workspace_session_name_metadata WHERE binding_id = ?1",
            [binding_id],
        )
        .map_err(|error| self.database_error("replace persisted session names", error))?;
        for record in session_names.records().values() {
            tx.execute(
                "INSERT INTO workspace_session_name_metadata
                    (binding_id, session_id, cwd, generated_name, session_name, display_name,
                     explicit)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    binding_id,
                    record.session_id,
                    record.cwd,
                    record.generated_name,
                    record.session_name,
                    record.display_name,
                    i64::from(record.explicit)
                ],
            )
            .map_err(|error| self.database_error("insert persisted session name", error))?;
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
            create_workspace_schema(&tx)?;
            migrate_workspace_space_icons(&tx)?;
            migrate_workspace_remote_ids(&tx)?;
            migrate_workspace_space_appearance(&tx)?;
            migrate_workspace_session_name_metadata(&tx)?;
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
        "SELECT space_id, binding_id, operation, session_id, old_name, new_name,
                display_name, explicit, cwd
         FROM workspace_pending_binding_operations ORDER BY space_id, binding_id",
    )?;
    let mut pending_scopes = HashSet::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ))
    })? {
        let (
            space_id,
            binding_id,
            operation,
            session_id,
            old_name,
            new_name,
            display_name,
            explicit,
            cwd,
        ) = row?;
        if space_id <= 0 || binding_id <= 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let scope = MuxScope::new(
            SpaceId::from_persistence(space_id),
            BindingId::from_persistence(binding_id),
        );
        if !scopes.contains(&scope)
            || !pending_scopes.insert(scope)
            || binding_membership_mutation_from_storage(
                &operation,
                session_id,
                old_name,
                new_name,
                display_name,
                explicit,
                cwd,
            )
            .is_err()
        {
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
    let valid_cwd = |cwd: &Option<String>| cwd.as_deref().is_none_or(|cwd| !cwd.contains('\0'));
    let valid = match mutation {
        BindingMembershipMutation::Create {
            display_name,
            explicit: _,
            cwd,
            ..
        } => valid_text(display_name) && valid_cwd(cwd),
        BindingMembershipMutation::Rename {
            display_name,
            explicit: _,
            cwd,
            ..
        } => valid_text(display_name) && valid_cwd(cwd),
        BindingMembershipMutation::Ditch { .. } => true,
    };
    valid
        .then_some(())
        .ok_or_else(|| WorkspacePersistenceError::new("binding membership mutation is invalid"))
}

struct StoredBindingMembershipMutation<'a> {
    operation: &'static str,
    session_id: &'a str,
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
            session_id,
            session_name,
            display_name,
            explicit,
            cwd,
        } => StoredBindingMembershipMutation {
            operation: "create",
            session_id,
            old_name: None,
            new_name: Some(session_name),
            display_name: Some(display_name),
            explicit: Some(*explicit),
            cwd: cwd.as_deref(),
        },
        BindingMembershipMutation::Rename {
            session_id,
            old_name,
            new_name,
            display_name,
            explicit,
            cwd,
        } => StoredBindingMembershipMutation {
            operation: "rename",
            session_id,
            old_name: Some(old_name),
            new_name: Some(new_name),
            display_name: Some(display_name),
            explicit: Some(*explicit),
            cwd: cwd.as_deref(),
        },
        BindingMembershipMutation::Ditch {
            session_id,
            old_name,
        } => StoredBindingMembershipMutation {
            operation: "ditch",
            session_id,
            old_name: Some(old_name),
            new_name: None,
            display_name: None,
            explicit: None,
            cwd: None,
        },
    }
}

fn binding_membership_mutation_from_storage(
    operation: &str,
    session_id: String,
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
    let mutation = match operation {
        "create" if old_name.is_none() => BindingMembershipMutation::Create {
            session_id,
            session_name: new_name.ok_or_else(|| {
                WorkspacePersistenceError::new("create binding membership is missing its name")
            })?,
            display_name: display_name.ok_or_else(|| {
                WorkspacePersistenceError::new(
                    "create binding membership is missing its display name",
                )
            })?,
            explicit: explicit.ok_or_else(|| {
                WorkspacePersistenceError::new(
                    "create binding membership is missing its explicit-name state",
                )
            })?,
            cwd,
        },
        "rename" => BindingMembershipMutation::Rename {
            session_id,
            old_name: old_name.ok_or_else(|| {
                WorkspacePersistenceError::new("rename binding membership is missing its old name")
            })?,
            new_name: new_name.ok_or_else(|| {
                WorkspacePersistenceError::new("rename binding membership is missing its new name")
            })?,
            display_name: display_name.ok_or_else(|| {
                WorkspacePersistenceError::new(
                    "rename binding membership is missing its display name",
                )
            })?,
            explicit: explicit.ok_or_else(|| {
                WorkspacePersistenceError::new(
                    "rename binding membership is missing its explicit-name state",
                )
            })?,
            cwd,
        },
        "ditch"
            if new_name.is_none()
                && display_name.is_none()
                && explicit.is_none()
                && cwd.is_none() =>
        {
            BindingMembershipMutation::Ditch {
                session_id,
                old_name: old_name.ok_or_else(|| {
                    WorkspacePersistenceError::new(
                        "ditch binding membership is missing its old name",
                    )
                })?,
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

fn apply_binding_membership_mutation(
    mutation: &BindingMembershipMutation,
    session_order: &mut SessionOrderStore,
    session_names: &mut SessionNameStore,
) -> WorkspaceResult<()> {
    match mutation {
        BindingMembershipMutation::Create {
            session_id,
            session_name,
            display_name,
            explicit,
            cwd,
        } => {
            if !session_order.add_session(session_name)
                && !session_order
                    .session_names()
                    .iter()
                    .any(|name| name == session_name)
            {
                return Err(WorkspacePersistenceError::new(
                    "apply binding membership: created session is already represented differently",
                ));
            }
            if let Some(cwd) = cwd {
                if *explicit {
                    session_names.mark_explicit(session_id, session_name, display_name, cwd);
                } else {
                    session_names.remember_generated(session_id, cwd, session_name, display_name);
                }
            }
        }
        BindingMembershipMutation::Rename {
            session_id,
            old_name,
            new_name,
            display_name,
            explicit,
            cwd,
        } => {
            let renamed = session_order.rename_session(old_name, new_name);
            let stored_names = session_order.session_names();
            let already_renamed = stored_names.iter().any(|name| name == new_name)
                && !stored_names.iter().any(|name| name == old_name);
            if !(renamed || already_renamed) {
                return Err(WorkspacePersistenceError::new(
                    "apply binding membership: renamed session is not represented",
                ));
            }
            let stored_cwd = cwd
                .clone()
                .or_else(|| {
                    session_names
                        .record(session_id)
                        .map(|record| record.cwd.clone())
                })
                .or_else(|| explicit.then(String::new));
            if let Some(cwd) = stored_cwd {
                let effective_session_id = if session_id == old_name {
                    new_name
                } else {
                    session_id
                };
                if effective_session_id != session_id {
                    session_names.remove_identity(session_id);
                }
                if *explicit {
                    session_names.mark_explicit(effective_session_id, new_name, display_name, &cwd);
                } else {
                    session_names.remember_generated(
                        effective_session_id,
                        &cwd,
                        new_name,
                        display_name,
                    );
                }
            }
        }
        BindingMembershipMutation::Ditch {
            session_id: _,
            old_name,
        } => {
            session_order.remove_session(old_name);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceSchemaKind {
    Fresh,
    LegacyTables,
    LegacyWorkspace,
    Current,
}

impl WorkspaceSchemaKind {
    fn uses_legacy_binding_cardinality(self) -> bool {
        matches!(self, Self::LegacyWorkspace)
    }

    fn allows_default_creation(self) -> bool {
        matches!(self, Self::Fresh | Self::LegacyTables)
    }
}

fn classify_schema(conn: &Connection, revision: i64) -> rusqlite::Result<WorkspaceSchemaKind> {
    let tables = user_tables(conn)?;
    if tables.is_empty() {
        return Ok(WorkspaceSchemaKind::Fresh);
    }

    let has_spaces = tables.contains("workspace_spaces");
    let has_bindings = tables.contains("workspace_bindings");
    if !has_spaces && !has_bindings {
        let legacy = ["session_groups", "sessions", "session_name_metadata"]
            .iter()
            .all(|table| tables.contains(*table));
        return legacy
            .then_some(WorkspaceSchemaKind::LegacyTables)
            .ok_or(rusqlite::Error::InvalidQuery);
    }
    if !has_spaces || !has_bindings {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let space_columns = table_columns(conn, "workspace_spaces")?;
    let binding_columns = table_columns(conn, "workspace_bindings")?;
    let required_space_columns = ["id", "name", "position"];
    let required_binding_columns = ["id", "space_id", "name", "backend", "hide_tmux_status"];
    if !required_space_columns
        .iter()
        .all(|column| space_columns.contains(*column))
        || !required_binding_columns
            .iter()
            .all(|column| binding_columns.contains(*column))
    {
        return Err(rusqlite::Error::InvalidQuery);
    }

    if revision == WORKSPACE_SNAPSHOT_REVISION {
        for (table, columns) in [
            (
                "workspace_spaces",
                [
                    "id",
                    "remote_id",
                    "name",
                    "icon",
                    "color",
                    "tint_sidebar",
                    "position",
                ]
                .as_slice(),
            ),
            (
                "workspace_bindings",
                [
                    "id",
                    "space_id",
                    "name",
                    "backend",
                    "hide_tmux_status",
                    "remote",
                    "unavailable",
                    "selected_session_id",
                    "selected_window_id",
                ]
                .as_slice(),
            ),
            (
                "workspace_session_groups",
                ["id", "binding_id", "name", "position"].as_slice(),
            ),
            (
                "workspace_sessions",
                ["binding_id", "name", "group_id", "position"].as_slice(),
            ),
            (
                "workspace_session_name_metadata",
                [
                    "binding_id",
                    "session_id",
                    "cwd",
                    "generated_name",
                    "session_name",
                    "display_name",
                    "explicit",
                ]
                .as_slice(),
            ),
            (
                "workspace_window_state",
                ["window_key", "selected_space_id"].as_slice(),
            ),
            (
                "workspace_pending_binding_operations",
                [
                    "space_id",
                    "binding_id",
                    "operation",
                    "session_id",
                    "old_name",
                    "new_name",
                    "display_name",
                    "explicit",
                    "cwd",
                ]
                .as_slice(),
            ),
        ] {
            if !tables.contains(table) {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let actual_columns = table_columns(conn, table)?;
            if !columns
                .iter()
                .all(|column| actual_columns.contains(*column))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(WorkspaceSchemaKind::Current)
    } else {
        Ok(WorkspaceSchemaKind::LegacyWorkspace)
    }
}

fn user_tables(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect()
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}

fn migrate_workspace_binding_cardinality(conn: &Connection) -> rusqlite::Result<()> {
    let table_sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'workspace_bindings'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let had_single_binding_constraint = table_sql.is_some_and(|sql| {
        sql.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
            .contains("space_id integer not null unique")
    });
    if !had_single_binding_constraint {
        return Ok(());
    }

    let columns = table_columns(conn, "workspace_bindings")?;
    let remote = if columns.contains("remote") {
        "remote"
    } else {
        "NULL"
    };
    let unavailable = if columns.contains("unavailable") {
        "unavailable"
    } else {
        "0"
    };
    let selected_session_id = if columns.contains("selected_session_id") {
        "selected_session_id"
    } else {
        "NULL"
    };
    let selected_window_id = if columns.contains("selected_window_id") {
        "selected_window_id"
    } else {
        "NULL"
    };

    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let migration = conn.execute_batch(&format!(
        "BEGIN IMMEDIATE;
         CREATE TABLE workspace_bindings_multiple (
             id INTEGER PRIMARY KEY,
             space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             backend TEXT NOT NULL,
             hide_tmux_status INTEGER NOT NULL,
             remote TEXT,
             unavailable INTEGER NOT NULL DEFAULT 0,
             selected_session_id TEXT,
             selected_window_id TEXT
         );
         INSERT INTO workspace_bindings_multiple
             (id, space_id, name, backend, hide_tmux_status, remote, unavailable,
              selected_session_id, selected_window_id)
         SELECT id, space_id, name, backend, hide_tmux_status, {remote}, {unavailable},
                {selected_session_id}, {selected_window_id}
         FROM workspace_bindings;
         DROP TABLE workspace_bindings;
         ALTER TABLE workspace_bindings_multiple RENAME TO workspace_bindings;
         COMMIT;"
    ));
    if migration.is_err() {
        let _ = conn.execute_batch("ROLLBACK;");
    }
    let foreign_keys = conn.pragma_update(None, "foreign_keys", "ON");
    migration?;
    foreign_keys
}

fn create_workspace_schema(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_spaces (
            id INTEGER PRIMARY KEY,
            remote_id TEXT UNIQUE,
            name TEXT NOT NULL,
            icon TEXT NOT NULL DEFAULT 'folder',
            color TEXT NOT NULL DEFAULT '#7AA2F7',
            tint_sidebar INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS workspace_bindings (
            id INTEGER PRIMARY KEY,
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            backend TEXT NOT NULL,
            hide_tmux_status INTEGER NOT NULL,
            remote TEXT,
            unavailable INTEGER NOT NULL DEFAULT 0,
            selected_session_id TEXT,
            selected_window_id TEXT
        );
        CREATE TABLE IF NOT EXISTS workspace_session_groups (
            id INTEGER PRIMARY KEY,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            position INTEGER NOT NULL,
            UNIQUE(binding_id, position)
        );
        CREATE TABLE IF NOT EXISTS workspace_sessions (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            group_id INTEGER NOT NULL REFERENCES workspace_session_groups(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            PRIMARY KEY(binding_id, name),
            UNIQUE(binding_id, group_id, position)
        );
        CREATE TABLE IF NOT EXISTS workspace_session_name_metadata (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            cwd TEXT NOT NULL,
            generated_name TEXT NOT NULL,
            session_name TEXT NOT NULL DEFAULT '',
            display_name TEXT NOT NULL DEFAULT '',
            explicit INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(binding_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS workspace_window_state (
            window_key TEXT PRIMARY KEY,
            selected_space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS workspace_pending_binding_operations (
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            operation TEXT NOT NULL,
            session_id TEXT NOT NULL,
            old_name TEXT,
            new_name TEXT,
            display_name TEXT,
            explicit INTEGER,
            cwd TEXT,
            PRIMARY KEY(space_id, binding_id)
        );",
    )
}

fn new_remote_space_id(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    tx.query_row(
        "SELECT lower(hex(randomblob(4))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(6)))",
        [],
        |row| row.get(0),
    )
}

fn migrate_workspace_remote_ids(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_spaces)")?;
    let has_remote_id = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "remote_id");
    drop(statement);
    if !has_remote_id {
        tx.execute("ALTER TABLE workspace_spaces ADD COLUMN remote_id TEXT", [])?;
    }
    tx.execute(
        "UPDATE workspace_spaces
         SET remote_id = lower(hex(randomblob(4))) || '-' ||
                         lower(hex(randomblob(2))) || '-' ||
                         lower(hex(randomblob(2))) || '-' ||
                         lower(hex(randomblob(2))) || '-' ||
                         lower(hex(randomblob(6)))
         WHERE remote_id IS NULL OR remote_id = ''",
        [],
    )?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS workspace_spaces_remote_id
         ON workspace_spaces(remote_id)",
        [],
    )?;
    Ok(())
}

fn migrate_workspace_space_icons(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_spaces)")?;
    let has_icon = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "icon");
    drop(statement);
    if !has_icon {
        tx.execute(
            "ALTER TABLE workspace_spaces ADD COLUMN icon TEXT NOT NULL DEFAULT 'folder'",
            [],
        )?;
    }
    Ok(())
}

fn migrate_workspace_space_appearance(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_spaces)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    if !columns.contains("color") {
        tx.execute(
            "ALTER TABLE workspace_spaces ADD COLUMN color TEXT NOT NULL DEFAULT '#7AA2F7'",
            [],
        )?;
    }
    if !columns.contains("tint_sidebar") {
        tx.execute(
            "ALTER TABLE workspace_spaces ADD COLUMN tint_sidebar INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

fn migrate_workspace_session_name_metadata(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_session_name_metadata)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    if !columns.contains("session_name") {
        tx.execute(
            "ALTER TABLE workspace_session_name_metadata
             ADD COLUMN session_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !columns.contains("display_name") {
        tx.execute(
            "ALTER TABLE workspace_session_name_metadata
             ADD COLUMN display_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    Ok(())
}

fn migrate_workspace_snapshot_state(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_bindings)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    if !columns.contains("unavailable") {
        tx.execute(
            "ALTER TABLE workspace_bindings
             ADD COLUMN unavailable INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains("selected_session_id") {
        tx.execute(
            "ALTER TABLE workspace_bindings ADD COLUMN selected_session_id TEXT",
            [],
        )?;
    }
    if !columns.contains("selected_window_id") {
        tx.execute(
            "ALTER TABLE workspace_bindings ADD COLUMN selected_window_id TEXT",
            [],
        )?;
    }
    if !columns.contains("remote") {
        tx.execute("ALTER TABLE workspace_bindings ADD COLUMN remote TEXT", [])?;
    }
    Ok(())
}

fn load_spaces(tx: &Transaction<'_>) -> rusqlite::Result<Vec<WorkspaceSpace>> {
    let mut statement = tx.prepare(
        "SELECT s.id, s.remote_id, s.name, s.icon, s.color, s.tint_sidebar, s.position,
                b.id, b.name, b.backend, b.hide_tmux_status, b.unavailable,
                b.selected_session_id, b.selected_window_id, b.remote
         FROM workspace_spaces s
         JOIN workspace_bindings b ON b.space_id = s.id
         ORDER BY s.position, s.id, b.id",
    )?;
    let rows = statement.query_map([], |row| {
        let space_id = row.get::<_, i64>(0)?;
        let binding_id = row.get::<_, i64>(7)?;
        if space_id <= 0 || binding_id <= 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let backend = backend_from_storage(&row.get::<_, String>(9)?)?;
        let remote = remote_from_storage(row.get::<_, Option<String>>(14)?.as_deref())?;
        let color = color_from_hex(&row.get::<_, String>(4)?)
            .ok_or_else(|| rusqlite::Error::InvalidQuery)?;
        let tint_sidebar = bool_from_storage(row.get::<_, i64>(5)?)?;
        let hide_tmux_status = bool_from_storage(row.get::<_, i64>(10)?)?;
        let unavailable = bool_from_storage(row.get::<_, i64>(11)?)?;
        let session_id = row.get::<_, Option<String>>(12)?;
        let window_id = row.get::<_, Option<String>>(13)?;
        if session_id.as_deref().is_some_and(str::is_empty)
            || (session_id.is_none() && window_id.is_some())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok((
            space_id,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            color,
            tint_sidebar,
            row.get::<_, i64>(6)?,
            WorkspaceBinding {
                scope: MuxScope::new(
                    SpaceId::from_persistence(space_id),
                    BindingId::from_persistence(binding_id),
                ),
                name: row.get(8)?,
                backend_override: backend,
                remote_override: remote,
                hide_tmux_status,
                unavailable,
                selection: session_id.map(|session_id| WorkspaceBindingSelection {
                    session_id,
                    window_id,
                }),
                session_order: SessionOrderStore::default(),
                session_names: SessionNameStore::default(),
            },
        ))
    })?;
    let mut spaces = Vec::<WorkspaceSpace>::new();
    for row in rows {
        let (space_id, remote_id, name, icon, color, tint_sidebar, position, binding) = row?;
        if let Some(space) = spaces.last_mut()
            && space.id.persistence_value() == space_id
        {
            space.bindings.push(binding);
        } else {
            spaces.push(WorkspaceSpace {
                id: SpaceId::from_persistence(space_id),
                remote_id,
                name,
                icon,
                color,
                tint_sidebar,
                position,
                bindings: vec![binding],
            });
        }
    }
    if spaces.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let stored_space_count = tx.query_row("SELECT COUNT(*) FROM workspace_spaces", [], |row| {
        row.get::<_, i64>(0)
    })?;
    if spaces.len() as i64 != stored_space_count {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let binding_ids = spaces
        .iter()
        .flat_map(|space| space.bindings.iter())
        .map(|binding| binding.scope.binding_id().persistence_value())
        .collect::<HashSet<_>>();
    let stored_binding_count =
        tx.query_row("SELECT COUNT(*) FROM workspace_bindings", [], |row| {
            row.get::<_, i64>(0)
        })?;
    if binding_ids.len() as i64 != stored_binding_count {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut groups = HashMap::<i64, Vec<(i64, SessionGroup, i64)>>::new();
    let mut group_ids = HashMap::<i64, i64>::new();
    let mut statement = tx.prepare(
        "SELECT id, binding_id, name, position
         FROM workspace_session_groups ORDER BY binding_id, position, id",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })? {
        let (group_id, binding_id, name, position) = row?;
        if group_id <= 0
            || !binding_ids.contains(&binding_id)
            || name.contains('\0')
            || position < 0
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if group_ids.insert(group_id, binding_id).is_some()
            || groups
                .entry(binding_id)
                .or_default()
                .iter()
                .any(|(_, _, existing_position)| *existing_position == position)
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        groups.entry(binding_id).or_default().push((
            group_id,
            SessionGroup {
                name,
                sessions: Vec::new(),
            },
            position,
        ));
    }

    let mut statement = tx.prepare(
        "SELECT binding_id, name, group_id, position
         FROM workspace_sessions ORDER BY binding_id, group_id, position",
    )?;
    let mut session_keys = HashSet::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })? {
        let (binding_id, name, group_id, position) = row?;
        if name.is_empty()
            || position < 0
            || !binding_ids.contains(&binding_id)
            || group_ids.get(&group_id) != Some(&binding_id)
            || !session_keys.insert((binding_id, name.clone()))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let Some((_, group, _)) = groups
            .get_mut(&binding_id)
            .and_then(|groups| groups.iter_mut().find(|(id, _, _)| *id == group_id))
        else {
            return Err(rusqlite::Error::InvalidQuery);
        };
        if group.sessions.len() != position as usize {
            return Err(rusqlite::Error::InvalidQuery);
        }
        group.sessions.push(name);
    }

    let mut names = HashMap::<i64, HashMap<String, SessionNameRecord>>::new();
    let mut name_cwds = HashSet::new();
    let mut statement = tx.prepare(
        "SELECT binding_id, session_id, cwd, generated_name, session_name, display_name, explicit
         FROM workspace_session_name_metadata ORDER BY binding_id, session_id",
    )?;
    for row in statement.query_map([], |row| {
        let explicit = bool_from_storage(row.get::<_, i64>(6)?)?;
        Ok((
            row.get::<_, i64>(0)?,
            SessionNameRecord {
                session_id: row.get(1)?,
                cwd: row.get(2)?,
                generated_name: row.get(3)?,
                session_name: row.get(4)?,
                display_name: row.get(5)?,
                explicit,
            },
        ))
    })? {
        let (binding_id, record) = row?;
        if !binding_ids.contains(&binding_id)
            || record.session_id.is_empty()
            || record.generated_name.is_empty()
            || (!record.cwd.is_empty() && !name_cwds.insert((binding_id, record.cwd.clone())))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if names
            .entry(binding_id)
            .or_default()
            .insert(record.session_id.clone(), record)
            .is_some()
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }

    for space in &mut spaces {
        if space.id.persistence_value() <= 0
            || space.name.trim().is_empty()
            || space.icon.trim().is_empty()
            || space.remote_id.trim().is_empty()
            || space.position < 0
            || space.bindings.is_empty()
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        for binding in &mut space.bindings {
            let binding_id = binding.scope.binding_id().persistence_value();
            let mut entries = groups.remove(&binding_id).unwrap_or_default();
            entries.sort_by_key(|(_, _, position)| *position);
            let order = entries
                .into_iter()
                .map(|(_, group, _)| group)
                .collect::<Vec<_>>();
            let has_groups = !order.is_empty();
            binding.session_order = SessionOrderStore::from_groups(order, has_groups);
            binding.session_names =
                SessionNameStore::from_records(names.remove(&binding_id).unwrap_or_default());
        }
    }
    if !groups.is_empty() || !names.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(spaces)
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
        session_order: SessionOrderStore::default(),
        session_names: SessionNameStore::default(),
    })
}

fn migrate_legacy_metadata(
    tx: &Transaction<'_>,
    binding_id: i64,
    path: &Path,
) -> rusqlite::Result<()> {
    let imported_sessions = if table_exists(tx, "session_groups")? && table_exists(tx, "sessions")?
    {
        let session_count: i64 =
            tx.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        if session_count == 0 {
            false
        } else {
            tx.execute(
                "INSERT INTO workspace_session_groups (binding_id, name, position)
                 SELECT ?1, name, position FROM session_groups ORDER BY position",
                [binding_id],
            )?;
            tx.execute(
                "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
                 SELECT ?1, old_session.name, scoped_group.id, old_session.position
                 FROM sessions old_session
                 JOIN session_groups old_group ON old_group.id = old_session.group_id
                 JOIN workspace_session_groups scoped_group
                   ON scoped_group.binding_id = ?1 AND scoped_group.position = old_group.position
                 ORDER BY old_group.position, old_session.position",
                [binding_id],
            )? > 0
        }
    } else {
        false
    };
    if !imported_sessions {
        migrate_legacy_order_file(tx, binding_id, path)?;
    }
    if table_exists(tx, "session_name_metadata")? {
        tx.execute(
            "INSERT INTO workspace_session_name_metadata
                 (binding_id, session_id, cwd, generated_name, session_name, explicit)
             SELECT ?1, session_id, cwd, generated_name, generated_name, explicit
             FROM session_name_metadata",
            [binding_id],
        )?;
    }
    Ok(())
}

fn migrate_legacy_order_file(
    tx: &Transaction<'_>,
    binding_id: i64,
    database_path: &Path,
) -> rusqlite::Result<()> {
    let Some(names) = legacy_order_paths(database_path)
        .into_iter()
        .find_map(|path| fs::read_to_string(path).ok())
    else {
        return Ok(());
    };
    let mut groups = Vec::<LegacySessionGroup>::new();
    let mut seen = HashSet::new();
    for name in names.lines().filter(|name| !name.is_empty()) {
        if !seen.insert(name) {
            continue;
        }
        let group_name = name.split_once('/').map_or("", |(group, _)| group);
        if group_name.is_empty() {
            groups.push(LegacySessionGroup {
                name: String::new(),
                sessions: vec![name.to_owned()],
            });
        } else if let Some(group) = groups.iter_mut().find(|group| group.name == group_name) {
            group.sessions.push(name.to_owned());
        } else if let Some(group) = groups
            .iter_mut()
            .find(|group| group.sessions.len() == 1 && group.sessions[0] == group_name)
        {
            group.name = group_name.to_owned();
            group.sessions.push(name.to_owned());
        } else {
            groups.push(LegacySessionGroup {
                name: group_name.to_owned(),
                sessions: vec![name.to_owned()],
            });
        }
    }
    for (group_position, group) in groups.iter().enumerate() {
        tx.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, ?2, ?3)",
            params![binding_id, group.name, group_position as i64],
        )?;
        let group_id = tx.last_insert_rowid();
        for (session_position, session) in group.sessions.iter().enumerate() {
            tx.execute(
                "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
                 VALUES (?1, ?2, ?3, ?4)",
                params![binding_id, session, group_id, session_position as i64],
            )?;
        }
    }
    Ok(())
}

fn legacy_order_paths(database_path: &Path) -> Vec<PathBuf> {
    let config_dir = database_path.parent().unwrap_or_else(|| Path::new("."));
    let mut paths = vec![config_dir.join("session-order")];
    if default_config_path().parent() == Some(config_dir) {
        paths.push(
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".config/tmux/session-order"),
        );
    }
    paths
}

struct LegacySessionGroup {
    name: String,
    sessions: Vec<String>,
}

fn table_exists(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

fn backend_to_storage(backend: Option<MultiplexerBackendConfig>) -> &'static str {
    match backend {
        None => "inherit",
        Some(MultiplexerBackendConfig::Rmux) => "rmux",
        Some(MultiplexerBackendConfig::Native) => "native",
        Some(MultiplexerBackendConfig::Tmux) => "tmux",
        Some(MultiplexerBackendConfig::Zellij) => "zellij",
    }
}

/// A binding's remote is stored as JSON rather than as columns of its own: it is one value the app
/// reads and writes whole, and every field it gained would otherwise be another migration.
fn remote_to_storage(remote: &SpaceRemoteOverride) -> rusqlite::Result<Option<String>> {
    match remote {
        SpaceRemoteOverride::Inherit => Ok(None),
        remote => serde_json::to_string(remote)
            .map(Some)
            .map_err(|_| rusqlite::Error::InvalidQuery),
    }
}

fn remote_from_storage(stored: Option<&str>) -> rusqlite::Result<SpaceRemoteOverride> {
    let Some(stored) = stored else {
        return Ok(SpaceRemoteOverride::Inherit);
    };
    serde_json::from_str(stored)
        .or_else(|_| serde_json::from_str(stored).map(SpaceRemoteOverride::Inline))
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn nonempty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn color_to_hex([red, green, blue]: [u8; 3]) -> String {
    format!("#{red:02X}{green:02X}{blue:02X}")
}

fn color_from_hex(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#')?;
    (value.len() == 6).then_some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

fn backend_from_storage(backend: &str) -> rusqlite::Result<Option<MultiplexerBackendConfig>> {
    match backend {
        "inherit" => Ok(None),
        "rmux" => Ok(Some(MultiplexerBackendConfig::Rmux)),
        "native" => Ok(Some(MultiplexerBackendConfig::Native)),
        "tmux" => Ok(Some(MultiplexerBackendConfig::Tmux)),
        "zellij" => Ok(Some(MultiplexerBackendConfig::Zellij)),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn bool_from_storage(value: i64) -> rusqlite::Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
