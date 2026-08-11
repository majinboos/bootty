//! Directory and Git-worktree identity plus advisory, cross-instance usage claims.
//!
//! This module deliberately does not create or close sessions.  A session launch records an
//! immutable launch claim, terminal cwd notifications update an observed claim, and worktree
//! removal uses the resulting advisory information only as a safety gate.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};

const LOCK_FILE_NAME: &str = ".directory-claims.lock";
const LOCK_RETRY: Duration = Duration::from_millis(10);
const LOCK_WAIT: Duration = Duration::from_secs(30);

static UNIQUE_SUFFIX: AtomicU64 = AtomicU64::new(0);
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

/// A Bootty process and generation.  IDs are opaque and must not be presented as display names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InstanceRef {
    pub instance_id: String,
    pub generation: u64,
}

/// A top-level application window within an instance.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WindowRef {
    pub instance: InstanceRef,
    pub window_id: String,
}

/// A backend binding scoped to one application window.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BindingRef {
    pub window: WindowRef,
    pub space_id: String,
    pub binding_id: String,
    pub generation: u64,
}

/// A backend session scoped to its binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionRef {
    pub binding: BindingRef,
    pub session_id: String,
}

/// A backend pane scoped to its binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PaneRef {
    pub binding: BindingRef,
    pub pane_id: String,
}

/// A terminal occupant scoped to its binding and process generation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TerminalRef {
    pub binding: BindingRef,
    pub terminal_id: String,
    pub occupant_generation: u64,
}

/// The three topology resources that own one directory claim.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClaimantRef {
    pub session: SessionRef,
    pub pane: PaneRef,
    pub terminal: TerminalRef,
}

impl ClaimantRef {
    /// Reject a claim assembled from resources in different bindings.
    pub fn validate(&self) -> Result<(), DirectoryClaimsError> {
        if self.session.binding != self.pane.binding {
            return Err(DirectoryClaimsError::InvalidClaimant {
                field: "session and pane must share a binding",
            });
        }
        if self.session.binding != self.terminal.binding {
            return Err(DirectoryClaimsError::InvalidClaimant {
                field: "session and terminal must share a binding",
            });
        }
        Ok(())
    }
}

/// The stable identity of a Git repository.  `common_git_dir` is shared by every linked
/// worktree; `root` is known only when it can be inferred without running Git.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepositoryRef {
    pub common_git_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
}

impl RepositoryRef {
    /// Repository equality intentionally ignores the optional, discoverable main-worktree root.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.common_git_dir == other.common_git_dir
    }
}

/// The process and caller that created a managed worktree.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorktreeCreator {
    pub instance: InstanceRef,
    pub caller: String,
}

/// One working tree in a repository.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorktreeRef {
    pub repository: RepositoryRef,
    pub git_dir: PathBuf,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorktreeCreator>,
    pub managed_by_bootty: bool,
}

impl WorktreeRef {
    /// The identity is independent of branch/head observations and management metadata.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.repository.same_identity(&other.repository)
            && self.git_dir == other.git_dir
            && self.path == other.path
    }

    /// Linked worktrees have a per-worktree Git directory rather than the repository common dir.
    pub fn is_linked(&self) -> bool {
        self.git_dir != self.repository.common_git_dir
    }
}

/// A canonical local path and, when observable from the filesystem, its Git identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DirectoryRef {
    pub canonical_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeRef>,
}

impl DirectoryRef {
    /// Resolve a local path without requiring its final component to exist.
    pub fn resolve(path: impl AsRef<Path>) -> io::Result<Self> {
        let canonical_path = canonicalize_local_path(path)?;
        let worktree = detect_worktree(&canonical_path);
        let repository = worktree
            .as_ref()
            .map(|worktree| worktree.repository.clone());
        Ok(Self {
            canonical_path,
            repository,
            worktree,
        })
    }

    /// Directory equality is canonical-path equality; Git metadata may legitimately change later.
    pub fn same_location(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path
    }
}

/// Why a directory is currently associated with a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryClaimSource {
    /// The cwd encoded in the immutable launch descriptor.
    Launch,
    /// The cwd last reported by the authoritative terminal runtime.
    Observed,
}

/// The user-facing severity of a permitted directory-sharing relationship.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryClaimSeverity {
    /// Repeated use inside the same session.
    Informational,
    /// A directory is also in use by another session.
    Warning,
    /// A linked Git worktree is also in use by another session.
    StrongWarning,
}

/// An advisory usage record.  Claims are deliberately many-to-many and never lock a directory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DirectoryClaim {
    pub directory: DirectoryRef,
    pub session: SessionRef,
    pub pane: PaneRef,
    pub terminal: TerminalRef,
    pub source: DirectoryClaimSource,
    pub since_revision: u64,
}

impl DirectoryClaim {
    /// Recover the claim's topology owner.
    pub fn claimant(&self) -> ClaimantRef {
        ClaimantRef {
            session: self.session.clone(),
            pane: self.pane.clone(),
            terminal: self.terminal.clone(),
        }
    }
}

/// The highest-precedence sharing condition for a prospective claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryClaimWarning {
    pub severity: DirectoryClaimSeverity,
    pub directory: DirectoryRef,
    pub conflicting_claims: Vec<DirectoryClaim>,
}

/// The result of recording a launch or observed cwd claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryClaimUpdate {
    pub revision: u64,
    pub claim: DirectoryClaim,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<DirectoryClaimWarning>,
}

/// The owner of one on-disk claims snapshot.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClaimOwner {
    pub instance_id: String,
    pub pid: u32,
    pub started_at_ms: u128,
}

impl ClaimOwner {
    /// Build an owner record for the current process.
    pub fn current(instance_id: impl Into<String>) -> io::Result<Self> {
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis();
        Ok(Self {
            instance_id: instance_id.into(),
            pid: std::process::id(),
            started_at_ms,
        })
    }
}

/// A serializable, atomically-published snapshot for one Bootty instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryClaimsSnapshot {
    pub owner: ClaimOwner,
    pub revision: u64,
    pub claims: Vec<DirectoryClaim>,
}

#[derive(Serialize)]
struct DirectoryClaimsSnapshotRef<'a> {
    owner: &'a ClaimOwner,
    revision: u64,
    claims: &'a [DirectoryClaim],
}

/// Determines whether an owner is proven dead.  Implementations must return `false` when they
/// cannot prove death; stale cleanup is intentionally conservative.
pub trait OwnerLiveness: Send + Sync {
    fn is_dead(&self, owner: &ClaimOwner) -> bool;
}

/// Process liveness backed by the operating-system process table.
#[derive(Debug, Default)]
pub struct SystemOwnerLiveness;

impl OwnerLiveness for SystemOwnerLiveness {
    fn is_dead(&self, owner: &ClaimOwner) -> bool {
        let system = sysinfo::System::new_all();
        system
            .process(sysinfo::Pid::from_u32(owner.pid))
            .is_none_or(|process| u128::from(process.start_time()) * 1000 > owner.started_at_ms)
    }
}

/// The active claims relevant to one worktree-removal decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemovalAssessment {
    pub worktree: WorktreeRef,
    pub active_claims: Vec<DirectoryClaim>,
    pub conflicting_claims: Vec<DirectoryClaim>,
}

impl WorktreeRemovalAssessment {
    /// Bind confirmation to exactly the worktree and cross-session claims that were reviewed.
    pub fn bound_confirmation(&self) -> Option<WorktreeRemovalConfirmation> {
        (!self.conflicting_claims.is_empty()).then(|| WorktreeRemovalConfirmation {
            worktree: self.worktree.clone(),
            conflicting_claims: self.conflicting_claims.clone(),
        })
    }
}

/// Explicit confirmation bound to a specific worktree and exact active claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemovalConfirmation {
    pub worktree: WorktreeRef,
    pub conflicting_claims: Vec<DirectoryClaim>,
}

/// Context passed to a worktree-removal attempt.  Session lifecycle remains outside this API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemovalRequest {
    /// Claims from this session are not considered cross-session conflicts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requester_session: Option<SessionRef>,
    /// Required when a final recheck finds a claim from another session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<WorktreeRemovalConfirmation>,
}

/// Errors returned by the claims seam.
#[derive(Debug)]
pub enum DirectoryClaimsError {
    Io(io::Error),
    StatePoisoned,
    InvalidOwner,
    InvalidClaimant {
        field: &'static str,
    },
    ImmutableLaunchIntent {
        claimant: Box<ClaimantRef>,
    },
    RevisionExhausted,
    LockTimeout {
        path: PathBuf,
    },
    UntrustedSnapshot {
        path: PathBuf,
    },
    ConfirmationRequired {
        assessment: Box<WorktreeRemovalAssessment>,
    },
    StaleConfirmation {
        assessment: Box<WorktreeRemovalAssessment>,
    },
    RemovalFailed {
        message: String,
    },
}

impl fmt::Display for DirectoryClaimsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "directory claims I/O failed: {error}"),
            Self::StatePoisoned => formatter.write_str("directory claims state lock was poisoned"),
            Self::InvalidOwner => {
                formatter.write_str("directory claim owner must have an instance id")
            }
            Self::InvalidClaimant { field } => {
                write!(formatter, "invalid directory claimant: {field}")
            }
            Self::ImmutableLaunchIntent { .. } => {
                formatter.write_str("a terminal's launch directory is immutable")
            }
            Self::RevisionExhausted => {
                formatter.write_str("directory claims revision counter exhausted")
            }
            Self::LockTimeout { path } => {
                write!(formatter, "timed out waiting for claims lock {path:?}")
            }
            Self::UntrustedSnapshot { path } => {
                write!(
                    formatter,
                    "claims snapshot {path:?} cannot be safely classified"
                )
            }
            Self::ConfirmationRequired { .. } => formatter
                .write_str("worktree removal needs confirmation for active cross-session claims"),
            Self::StaleConfirmation { .. } => formatter
                .write_str("worktree removal confirmation does not match the final active claims"),
            Self::RemovalFailed { message } => {
                write!(formatter, "worktree removal failed: {message}")
            }
        }
    }
}

impl std::error::Error for DirectoryClaimsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for DirectoryClaimsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Canonicalize a local path while retaining a normalized absolute path for a nonexistent tail.
pub fn canonicalize_local_path(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return Ok(canonical);
    }

    let mut resolved = PathBuf::new();
    let mut nonexistent_tail = Vec::new();

    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            std::path::Component::RootDir => resolved.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if nonexistent_tail.pop().is_some() {
                    continue;
                }
                let parent = resolved.join("..");
                match fs::canonicalize(parent) {
                    Ok(parent) => resolved = parent,
                    Err(_) => return Ok(normalize_absolute_path(&absolute)),
                }
            }
            std::path::Component::Normal(component) if nonexistent_tail.is_empty() => {
                let candidate = resolved.join(component);
                match fs::canonicalize(candidate) {
                    Ok(candidate) => resolved = candidate,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                        ) =>
                    {
                        nonexistent_tail.push(component.to_owned());
                    }
                    // A permission or I/O failure should not make an otherwise usable local
                    // reference disappear.  The normalized absolute form remains stable and
                    // serializable.
                    Err(_) => return Ok(normalize_absolute_path(&absolute)),
                }
            }
            std::path::Component::Normal(component) => nonexistent_tail.push(component.to_owned()),
        }
    }

    for component in nonexistent_tail {
        resolved.push(component);
    }
    Ok(resolved)
}

/// Default owner-private location for per-instance directory-claim snapshots.
pub fn default_claims_directory() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no user runtime directory"))?;
    Ok(base.join("bootty").join("directory-claims"))
}

/// Cloneable claims interface shared by AppState and extensions.
#[derive(Clone)]
pub struct DirectoryClaims {
    inner: Arc<DirectoryClaimsInner>,
}

struct DirectoryClaimsInner {
    snapshots: SnapshotStore,
    state: Mutex<ClaimState>,
}

#[derive(Clone, Default)]
struct ClaimState {
    revision: u64,
    claims: Vec<DirectoryClaim>,
}

impl ClaimState {
    fn next_revision(&mut self) -> Result<u64, DirectoryClaimsError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(DirectoryClaimsError::RevisionExhausted)?;
        Ok(self.revision)
    }

    fn snapshot(&self, owner: ClaimOwner) -> DirectoryClaimsSnapshot {
        DirectoryClaimsSnapshot {
            owner,
            revision: self.revision,
            claims: self.claims.clone(),
        }
    }
}

impl DirectoryClaims {
    /// Open claims storage in the standard owner-private runtime directory.
    pub fn open(owner: ClaimOwner) -> Result<Self, DirectoryClaimsError> {
        Self::at(default_claims_directory()?, owner)
    }

    /// Open claims storage at an explicit owner-private directory.
    pub fn at(
        snapshot_directory: impl Into<PathBuf>,
        owner: ClaimOwner,
    ) -> Result<Self, DirectoryClaimsError> {
        Self::at_with_liveness(snapshot_directory, owner, Arc::new(SystemOwnerLiveness))
    }

    /// Open claims storage with an explicit liveness proof source.  This is useful to embedders
    /// that already own process discovery and for deterministic integration tests.
    pub fn at_with_liveness(
        snapshot_directory: impl Into<PathBuf>,
        owner: ClaimOwner,
        liveness: Arc<dyn OwnerLiveness>,
    ) -> Result<Self, DirectoryClaimsError> {
        if owner.instance_id.is_empty() {
            return Err(DirectoryClaimsError::InvalidOwner);
        }

        let snapshots = SnapshotStore {
            directory: snapshot_directory.into(),
            owner,
            liveness,
        };
        snapshots.prepare_directory()?;

        let initial_snapshot = {
            let _lock = snapshots.acquire_global_lock()?;
            snapshots.read_own_snapshot_locked()?
        };
        let should_publish = initial_snapshot.is_none();
        let state = match initial_snapshot {
            Some(snapshot) => {
                let revision = snapshot.revision.max(
                    snapshot
                        .claims
                        .iter()
                        .map(|claim| claim.since_revision)
                        .max()
                        .unwrap_or(0),
                );
                let mut claims = snapshot.claims;
                claims.sort();
                ClaimState { revision, claims }
            }
            None => ClaimState::default(),
        };
        let claims = Self {
            inner: Arc::new(DirectoryClaimsInner {
                snapshots,
                state: Mutex::new(state),
            }),
        };
        if should_publish {
            claims.publish_current()?;
        }
        Ok(claims)
    }

    /// Record an immutable cwd from a session-launch descriptor.
    pub fn record_launch(
        &self,
        claimant: ClaimantRef,
        directory: DirectoryRef,
    ) -> Result<DirectoryClaimUpdate, DirectoryClaimsError> {
        claimant.validate()?;
        let mut state = self.state()?;

        if let Some(existing) = state.claims.iter().find(|claim| {
            claim.source == DirectoryClaimSource::Launch && has_claimant(claim, &claimant)
        }) {
            if !existing.directory.same_location(&directory) {
                return Err(DirectoryClaimsError::ImmutableLaunchIntent {
                    claimant: Box::new(claimant),
                });
            }
            return Ok(DirectoryClaimUpdate {
                revision: state.revision,
                claim: existing.clone(),
                warning: None,
            });
        }

        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let warning = self.warning_from_active_locked(&claimant, &directory)?;
        let mut next = state.clone();
        let revision = next.next_revision()?;
        let claim = DirectoryClaim {
            directory,
            session: claimant.session,
            pane: claimant.pane,
            terminal: claimant.terminal,
            source: DirectoryClaimSource::Launch,
            since_revision: revision,
        };
        next.claims.push(claim.clone());
        next.claims.sort();
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(DirectoryClaimUpdate {
            revision,
            claim,
            warning,
        })
    }

    /// Record an authoritative terminal cwd observation.  This only replaces an observed claim;
    /// it never edits the terminal's launch intent.
    pub fn observe_cwd(
        &self,
        claimant: ClaimantRef,
        directory: DirectoryRef,
    ) -> Result<DirectoryClaimUpdate, DirectoryClaimsError> {
        claimant.validate()?;
        let mut state = self.state()?;

        if let Some(existing) = state.claims.iter().find(|claim| {
            claim.source == DirectoryClaimSource::Observed && has_claimant(claim, &claimant)
        }) && existing.directory.same_location(&directory)
        {
            return Ok(DirectoryClaimUpdate {
                revision: state.revision,
                claim: existing.clone(),
                warning: None,
            });
        }

        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let warning = self.warning_from_active_locked(&claimant, &directory)?;
        let mut next = state.clone();
        next.claims.retain(|claim| {
            !(claim.source == DirectoryClaimSource::Observed && claim.terminal == claimant.terminal)
        });
        let revision = next.next_revision()?;
        let claim = DirectoryClaim {
            directory,
            session: claimant.session,
            pane: claimant.pane,
            terminal: claimant.terminal,
            source: DirectoryClaimSource::Observed,
            since_revision: revision,
        };
        next.claims.push(claim.clone());
        next.claims.sort();
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(DirectoryClaimUpdate {
            revision,
            claim,
            warning,
        })
    }

    /// Drop every launch/observed claim for one exact session, pane, and terminal occupant. A
    /// close event must preserve claims for a later occupant that reused this terminal slot.
    pub fn release_claimant(
        &self,
        claimant: &ClaimantRef,
    ) -> Result<Option<u64>, DirectoryClaimsError> {
        claimant.validate()?;
        let mut state = self.state()?;
        if !state
            .claims
            .iter()
            .any(|claim| has_claimant(claim, claimant))
        {
            return Ok(None);
        }

        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let mut next = state.clone();
        next.claims.retain(|claim| !has_claimant(claim, claimant));
        let revision = next.next_revision()?;
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(Some(revision))
    }

    /// Drop an old occupant's observed cwd without rewriting its immutable launch intent. An
    /// authoritative occupant-replacement event supplies this exact claimant before a new
    /// occupant begins reporting cwd.
    pub fn release_observed_claimant(
        &self,
        claimant: &ClaimantRef,
    ) -> Result<Option<u64>, DirectoryClaimsError> {
        claimant.validate()?;
        let mut state = self.state()?;
        if !state.claims.iter().any(|claim| {
            claim.source == DirectoryClaimSource::Observed && has_claimant(claim, claimant)
        }) {
            return Ok(None);
        }

        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let mut next = state.clone();
        next.claims.retain(|claim| {
            !(claim.source == DirectoryClaimSource::Observed && has_claimant(claim, claimant))
        });
        let revision = next.next_revision()?;
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(Some(revision))
    }

    /// Drop every launch/observed claim owned by one exact application window. The instance
    /// generation is part of `window`, so teardown from an older logical owner cannot remove a
    /// replacement window's claims.
    pub fn release_window_claims(
        &self,
        window: &WindowRef,
    ) -> Result<Option<u64>, DirectoryClaimsError> {
        let mut state = self.state()?;
        if !state
            .claims
            .iter()
            .any(|claim| has_window_claim(claim, window))
        {
            return Ok(None);
        }

        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let mut next = state.clone();
        next.claims.retain(|claim| !has_window_claim(claim, window));
        let revision = next.next_revision()?;
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(Some(revision))
    }

    /// Reconcile one binding's claims after an authoritative topology refresh. Closed terminal
    /// slots lose every claim; a live replacement retains the prior immutable launch intent while
    /// dropping its predecessor's non-authoritative observed cwd.
    pub(crate) fn reconcile_live_claimants(
        &self,
        binding: &BindingRef,
        live_claimants: impl IntoIterator<Item = ClaimantRef>,
    ) -> Result<Option<u64>, DirectoryClaimsError> {
        let live_claimants = live_claimants.into_iter().collect::<Vec<_>>();
        let mut state = self.state()?;
        let is_stale = |claim: &DirectoryClaim| {
            if !same_binding_slot(&claim.terminal.binding, binding) {
                return false;
            }
            match claim.source {
                DirectoryClaimSource::Launch => !live_claimants
                    .iter()
                    .any(|live| same_claimant_slot(claim, live)),
                DirectoryClaimSource::Observed => {
                    !live_claimants.iter().any(|live| has_claimant(claim, live))
                }
            }
        };
        if !state.claims.iter().any(&is_stale) {
            return Ok(None);
        }

        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let mut next = state.clone();
        next.claims.retain(|claim| !is_stale(claim));
        let revision = next.next_revision()?;
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(Some(revision))
    }

    /// Rebase claims from retired binding generations against one authoritative topology
    /// snapshot. Absent slots lose every claim, retired observed cwd claims are discarded, and a
    /// retained launch claim adopts the live slot's exact binding and occupant generations without
    /// changing its immutable directory or source.
    pub(crate) fn rebase_retired_binding_claims(
        &self,
        binding: &BindingRef,
        live_claimants: impl IntoIterator<Item = ClaimantRef>,
    ) -> Result<Option<u64>, DirectoryClaimsError> {
        let live_claimants = live_claimants.into_iter().collect::<Vec<_>>();
        for claimant in &live_claimants {
            claimant.validate()?;
            if claimant.terminal.binding != *binding {
                return Err(DirectoryClaimsError::InvalidClaimant {
                    field: "live claimant must use the current binding",
                });
            }
        }

        let mut state = self.state()?;
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let mut rebased_claims = Vec::with_capacity(state.claims.len());
        for claim in &state.claims {
            if !same_binding_slot(&claim.terminal.binding, binding) {
                rebased_claims.push(claim.clone());
                continue;
            }
            let Some(live) = live_claimants
                .iter()
                .find(|live| same_claimant_slot(claim, live))
            else {
                continue;
            };

            match claim.source {
                DirectoryClaimSource::Launch
                    if claim.terminal.binding.generation != binding.generation =>
                {
                    rebased_claims.push(DirectoryClaim {
                        session: live.session.clone(),
                        pane: live.pane.clone(),
                        terminal: live.terminal.clone(),
                        ..claim.clone()
                    });
                }
                DirectoryClaimSource::Launch => rebased_claims.push(claim.clone()),
                DirectoryClaimSource::Observed if has_claimant(claim, live) => {
                    rebased_claims.push(claim.clone());
                }
                DirectoryClaimSource::Observed => {}
            }
        }
        rebased_claims.sort();
        if rebased_claims == state.claims {
            return Ok(None);
        }

        let mut next = ClaimState {
            revision: state.revision,
            claims: rebased_claims,
        };
        let revision = next.next_revision()?;
        self.inner.snapshots.write_snapshot_locked(&next)?;
        *state = next;
        Ok(Some(revision))
    }

    /// Run an operation while the global claims lock protects one complete
    /// inventory of live owners. The callback must not call back into this
    /// `DirectoryClaims`, because the lock is deliberately non-reentrant.
    pub(crate) fn with_live_snapshots<T>(
        &self,
        operation: impl FnOnce(&[DirectoryClaimsSnapshot]) -> T,
    ) -> Result<T, DirectoryClaimsError> {
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let (snapshots, _) = self.inner.snapshots.read_live_snapshots_locked()?;
        Ok(operation(&snapshots))
    }

    /// Return the latest immutable snapshot from every live owner under one
    /// global-lock acquisition. Local snapshots retain their existing
    /// per-owner shape; callers that need a complete inventory must use this
    /// rather than [`Self::snapshot`].
    pub fn live_snapshots(&self) -> Result<Vec<DirectoryClaimsSnapshot>, DirectoryClaimsError> {
        self.with_live_snapshots(|snapshots| snapshots.to_vec())
    }

    /// Return this instance's current snapshot without reading other processes' state.
    pub fn snapshot(&self) -> Result<DirectoryClaimsSnapshot, DirectoryClaimsError> {
        let state = self.state()?;
        Ok(state.snapshot(self.inner.snapshots.owner.clone()))
    }

    /// Return all live, discoverable claims for one exact canonical directory.
    pub fn claims_for(
        &self,
        directory: &DirectoryRef,
    ) -> Result<Vec<DirectoryClaim>, DirectoryClaimsError> {
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let (snapshots, _) = self.inner.snapshots.read_live_snapshots_locked()?;
        let mut claims = snapshots
            .into_iter()
            .flat_map(|snapshot| snapshot.claims)
            .filter(|claim| claim.directory.same_location(directory))
            .collect::<Vec<_>>();
        claims.sort();
        Ok(claims)
    }

    /// Return every live, discoverable claim belonging to one exact worktree.
    /// Claims may name arbitrary subdirectories, so this intentionally matches
    /// worktree identity rather than only the worktree root path.
    pub fn claims_for_worktree(
        &self,
        worktree: &WorktreeRef,
    ) -> Result<Vec<DirectoryClaim>, DirectoryClaimsError> {
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let (snapshots, _) = self.inner.snapshots.read_live_snapshots_locked()?;
        let mut claims = snapshots
            .into_iter()
            .flat_map(|snapshot| snapshot.claims)
            .filter(|claim| {
                claim
                    .directory
                    .worktree
                    .as_ref()
                    .is_some_and(|claimed| claimed.same_identity(worktree))
            })
            .collect::<Vec<_>>();
        claims.sort();
        Ok(claims)
    }

    /// Evaluate the highest-precedence permitted sharing warning for a prospective claim.
    pub fn sharing_warning(
        &self,
        claimant: &ClaimantRef,
        directory: &DirectoryRef,
    ) -> Result<Option<DirectoryClaimWarning>, DirectoryClaimsError> {
        claimant.validate()?;
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        self.warning_from_active_locked(claimant, directory)
    }

    /// Delete only snapshots whose owners are proven dead.  Unknown/malformed snapshots fail
    /// closed and are left untouched.
    pub fn cleanup_stale_snapshots(&self) -> Result<usize, DirectoryClaimsError> {
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let (_, removed) = self.inner.snapshots.read_live_snapshots_locked()?;
        Ok(removed)
    }

    /// Read a current worktree-removal assessment.  Callers may use its bound confirmation in a
    /// later removal attempt, but that attempt always performs a final locked reread.
    pub fn assess_worktree_removal(
        &self,
        worktree: &WorktreeRef,
        requester_session: Option<&SessionRef>,
    ) -> Result<WorktreeRemovalAssessment, DirectoryClaimsError> {
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        self.assess_worktree_removal_locked(worktree, requester_session)
    }

    /// Execute the supplied Git/worktree remover only after a final, globally locked snapshot
    /// reread.  The callback runs under the same lock, so every publisher is serialized with the
    /// safety decision.  It must not call back into this `DirectoryClaims` instance.
    pub fn remove_worktree(
        &self,
        worktree: &WorktreeRef,
        request: &WorktreeRemovalRequest,
        remove: impl FnOnce(&WorktreeRef) -> Result<(), String>,
    ) -> Result<WorktreeRemovalAssessment, DirectoryClaimsError> {
        // Every local publisher obtains state before the global lock.  Preserve that ordering so
        // a concurrent observed-cwd update cannot slip between final recheck and removal.
        let _state = self.state()?;
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        let assessment =
            self.assess_worktree_removal_locked(worktree, request.requester_session.as_ref())?;

        if !assessment.conflicting_claims.is_empty() {
            let Some(confirmation) = request.confirmation.as_ref() else {
                return Err(DirectoryClaimsError::ConfirmationRequired {
                    assessment: Box::new(assessment),
                });
            };
            if !confirmation_matches(confirmation, &assessment) {
                return Err(DirectoryClaimsError::StaleConfirmation {
                    assessment: Box::new(assessment),
                });
            }
        }

        remove(worktree).map_err(|message| DirectoryClaimsError::RemovalFailed { message })?;
        Ok(assessment)
    }

    fn publish_current(&self) -> Result<(), DirectoryClaimsError> {
        let state = self.state()?;
        let _lock = self.inner.snapshots.acquire_global_lock()?;
        self.inner.snapshots.write_snapshot_locked(&state)
    }

    fn warning_from_active_locked(
        &self,
        claimant: &ClaimantRef,
        directory: &DirectoryRef,
    ) -> Result<Option<DirectoryClaimWarning>, DirectoryClaimsError> {
        let (snapshots, _) = self.inner.snapshots.read_live_snapshots_locked()?;
        Ok(sharing_warning(
            snapshots.into_iter().flat_map(|snapshot| snapshot.claims),
            claimant,
            directory,
        ))
    }

    fn assess_worktree_removal_locked(
        &self,
        worktree: &WorktreeRef,
        requester_session: Option<&SessionRef>,
    ) -> Result<WorktreeRemovalAssessment, DirectoryClaimsError> {
        let (snapshots, _) = self.inner.snapshots.read_live_snapshots_locked()?;
        let mut active_claims = snapshots
            .into_iter()
            .flat_map(|snapshot| snapshot.claims)
            .filter(|claim| {
                claim
                    .directory
                    .worktree
                    .as_ref()
                    .is_some_and(|claimed| claimed.same_identity(worktree))
            })
            .collect::<Vec<_>>();
        active_claims.sort();
        let conflicting_claims = active_claims
            .iter()
            .filter(|claim| match requester_session {
                Some(session) => &claim.session != session,
                None => true,
            })
            .cloned()
            .collect();
        Ok(WorktreeRemovalAssessment {
            worktree: worktree.clone(),
            active_claims,
            conflicting_claims,
        })
    }

    fn state(&self) -> Result<MutexGuard<'_, ClaimState>, DirectoryClaimsError> {
        self.inner
            .state
            .lock()
            .map_err(|_| DirectoryClaimsError::StatePoisoned)
    }
}

fn confirmation_matches(
    confirmation: &WorktreeRemovalConfirmation,
    assessment: &WorktreeRemovalAssessment,
) -> bool {
    confirmation.worktree.same_identity(&assessment.worktree)
        && confirmation.conflicting_claims == assessment.conflicting_claims
}

fn has_claimant(claim: &DirectoryClaim, claimant: &ClaimantRef) -> bool {
    claim.session == claimant.session
        && claim.pane == claimant.pane
        && claim.terminal == claimant.terminal
}

/// Match every nested binding so an invalid persisted claim cannot make window teardown erase a
/// partially overlapping live claim.
fn has_window_claim(claim: &DirectoryClaim, window: &WindowRef) -> bool {
    claim.session.binding.window == *window
        && claim.pane.binding.window == *window
        && claim.terminal.binding.window == *window
}

/// Terminal reuse advances only its occupant generation. Launch and observed claims belonging to
/// different generations of the same terminal slot describe one user-visible terminal, not a
/// directory-sharing relationship.
fn same_terminal_slot(left: &TerminalRef, right: &TerminalRef) -> bool {
    left.binding == right.binding && left.terminal_id == right.terminal_id
}

/// A reconnect rebinds the same logical terminal topology under a new binding
/// generation. Launch intent belongs to that topology slot; an observed cwd
/// belongs to one exact occupant.
fn same_claimant_slot(claim: &DirectoryClaim, claimant: &ClaimantRef) -> bool {
    same_binding_slot(&claim.session.binding, &claimant.session.binding)
        && claim.session.session_id == claimant.session.session_id
        && same_binding_slot(&claim.pane.binding, &claimant.pane.binding)
        && claim.pane.pane_id == claimant.pane.pane_id
        && same_binding_slot(&claim.terminal.binding, &claimant.terminal.binding)
        && claim.terminal.terminal_id == claimant.terminal.terminal_id
}

fn same_claimant_terminal_slot(claim: &DirectoryClaim, claimant: &ClaimantRef) -> bool {
    claim.session == claimant.session
        && claim.pane == claimant.pane
        && same_terminal_slot(&claim.terminal, &claimant.terminal)
}

fn same_binding_slot(left: &BindingRef, right: &BindingRef) -> bool {
    left.window == right.window
        && left.space_id == right.space_id
        && left.binding_id == right.binding_id
}

fn sharing_warning(
    claims: impl IntoIterator<Item = DirectoryClaim>,
    claimant: &ClaimantRef,
    directory: &DirectoryRef,
) -> Option<DirectoryClaimWarning> {
    let mut severity = None;
    let mut conflicting_claims = Vec::new();

    for claim in claims {
        // A replacement keeps its predecessor's immutable launch intent. Its distinct occupant
        // generation still describes the same terminal slot, but a different session/pane does
        // not.
        if same_claimant_terminal_slot(&claim, claimant) {
            continue;
        }

        let same_directory = claim.directory.same_location(directory);
        let same_linked_worktree = claim
            .directory
            .worktree
            .as_ref()
            .zip(directory.worktree.as_ref())
            .is_some_and(|(left, right)| {
                left.is_linked() && right.is_linked() && left.same_identity(right)
            });
        if !same_directory && !same_linked_worktree {
            continue;
        }

        let claim_severity = if claim.session == claimant.session {
            DirectoryClaimSeverity::Informational
        } else if same_linked_worktree {
            DirectoryClaimSeverity::StrongWarning
        } else {
            DirectoryClaimSeverity::Warning
        };

        match severity {
            None => {
                severity = Some(claim_severity);
                conflicting_claims.push(claim);
            }
            Some(current) if claim_severity > current => {
                severity = Some(claim_severity);
                conflicting_claims.clear();
                conflicting_claims.push(claim);
            }
            Some(current) if claim_severity == current => conflicting_claims.push(claim),
            Some(_) => {}
        }
    }

    let severity = severity?;
    conflicting_claims.sort();
    Some(DirectoryClaimWarning {
        severity,
        directory: directory.clone(),
        conflicting_claims,
    })
}

#[derive(Clone)]
struct SnapshotStore {
    directory: PathBuf,
    owner: ClaimOwner,
    liveness: Arc<dyn OwnerLiveness>,
}

impl SnapshotStore {
    fn prepare_directory(&self) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        set_owner_only_directory(&self.directory)
    }

    fn snapshot_path(&self, revision: u64) -> PathBuf {
        self.directory
            .join(snapshot_file_name(&self.owner, revision))
    }

    fn acquire_global_lock(&self) -> Result<GlobalLock, DirectoryClaimsError> {
        self.prepare_directory()?;
        GlobalLock::acquire(&self.directory)
    }

    fn read_own_snapshot_locked(
        &self,
    ) -> Result<Option<DirectoryClaimsSnapshot>, DirectoryClaimsError> {
        let (snapshots, _) = self.read_snapshots_locked(false)?;
        Ok(snapshots
            .into_iter()
            .find(|snapshot| snapshot.owner == self.owner))
    }

    fn write_snapshot_locked(&self, state: &ClaimState) -> Result<(), DirectoryClaimsError> {
        let snapshot = DirectoryClaimsSnapshotRef {
            owner: &self.owner,
            revision: state.revision,
            claims: &state.claims,
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(json_to_io)?;
        let target = self.snapshot_path(state.revision);
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            snapshot_file_name(&self.owner, state.revision),
            unique_suffix()
        ));
        write_owner_only_file(&temporary, &bytes)?;

        match fs::hard_link(&temporary, &target) {
            Ok(()) => {
                let _ = fs::remove_file(&temporary);
                // The revision link is the publication point.  Persist it before pruning any
                // older recoverable snapshot, and never roll back in-memory state after it exists.
                if sync_snapshot_directory(&self.directory).is_ok() {
                    self.remove_superseded_own_snapshots_locked(state.revision);
                    let _ = sync_snapshot_directory(&self.directory);
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = fs::read(&target);
                let _ = fs::remove_file(&temporary);
                match existing {
                    Ok(existing) if existing == bytes => Ok(()),
                    Ok(_) => Err(error.into()),
                    Err(error) => Err(error.into()),
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error.into())
            }
        }
    }

    #[cfg(unix)]
    fn remove_superseded_own_snapshots_locked(&self, current_revision: u64) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Ok(snapshot) = fs::read(&path).and_then(|bytes| {
                serde_json::from_slice::<DirectoryClaimsSnapshot>(&bytes).map_err(json_to_io)
            }) else {
                continue;
            };
            if snapshot.owner == self.owner
                && snapshot.revision < current_revision
                && entry.file_name()
                    == std::ffi::OsString::from(snapshot_file_name(
                        &snapshot.owner,
                        snapshot.revision,
                    ))
            {
                let _ = fs::remove_file(path);
            }
        }
    }

    #[cfg(not(unix))]
    fn remove_superseded_own_snapshots_locked(&self, _current_revision: u64) {
        let _ = self;
        // std cannot durably sync a directory on Windows.  Retain old immutable revisions until
        // stale-owner cleanup so a power loss cannot leave an owner with no recoverable snapshot.
    }

    fn read_snapshots_locked(
        &self,
        remove_stale: bool,
    ) -> Result<(Vec<DirectoryClaimsSnapshot>, usize), DirectoryClaimsError> {
        let mut snapshots = BTreeMap::new();
        let mut removed = 0;
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Vec::new(), removed));
            }
            Err(error) => return Err(error.into()),
        };

        for entry in entries {
            let entry = entry.map_err(DirectoryClaimsError::from)?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|_| DirectoryClaimsError::UntrustedSnapshot { path: path.clone() })?;
            if !file_type.is_file() {
                return Err(DirectoryClaimsError::UntrustedSnapshot { path });
            }
            let bytes = fs::read(&path)
                .map_err(|_| DirectoryClaimsError::UntrustedSnapshot { path: path.clone() })?;
            let snapshot =
                serde_json::from_slice::<DirectoryClaimsSnapshot>(&bytes).map_err(|_| {
                    // The final-removal protocol must fail closed when it cannot classify a live
                    // snapshot.  Leave the file untouched because it has no proven stale owner.
                    DirectoryClaimsError::UntrustedSnapshot { path: path.clone() }
                })?;
            if snapshot
                .claims
                .iter()
                .any(|claim| claim.since_revision > snapshot.revision)
            {
                return Err(DirectoryClaimsError::UntrustedSnapshot { path });
            }
            let expected_name =
                std::ffi::OsString::from(snapshot_file_name(&snapshot.owner, snapshot.revision));
            if entry.file_name() != expected_name {
                return Err(DirectoryClaimsError::UntrustedSnapshot { path });
            }
            if remove_stale && self.liveness.is_dead(&snapshot.owner) {
                match fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                continue;
            }
            if snapshots
                .get(&snapshot.owner)
                .is_none_or(|current: &DirectoryClaimsSnapshot| {
                    current.revision < snapshot.revision
                })
            {
                snapshots.insert(snapshot.owner.clone(), snapshot);
            }
        }
        Ok((snapshots.into_values().collect(), removed))
    }

    fn read_live_snapshots_locked(
        &self,
    ) -> Result<(Vec<DirectoryClaimsSnapshot>, usize), DirectoryClaimsError> {
        self.read_snapshots_locked(true)
    }
}

struct GlobalLock {
    _process_guard: MutexGuard<'static, ()>,
    _file: File,
}

impl GlobalLock {
    fn acquire(directory: &Path) -> Result<Self, DirectoryClaimsError> {
        let process_guard = PROCESS_LOCK
            .lock()
            .map_err(|_| DirectoryClaimsError::StatePoisoned)?;
        let lock_path = directory.join(LOCK_FILE_NAME);
        let file = open_owner_only_lock_file(&lock_path)?;
        let began = Instant::now();

        loop {
            match FileExt::try_lock(&file) {
                Ok(()) => {
                    return Ok(Self {
                        _process_guard: process_guard,
                        _file: file,
                    });
                }
                Err(TryLockError::WouldBlock) if began.elapsed() < LOCK_WAIT => {
                    thread::sleep(LOCK_RETRY);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(DirectoryClaimsError::LockTimeout { path: lock_path });
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
}

fn open_owner_only_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_owner_only_file(path)?;
    Ok(file)
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                }
            }
            std::path::Component::Normal(component) => normalized.push(component),
        }
    }
    normalized
}

fn detect_worktree(canonical_path: &Path) -> Option<WorktreeRef> {
    for worktree_path in canonical_path.ancestors() {
        let marker = worktree_path.join(".git");
        let Some(git_dir) = git_directory_from_marker(&marker, worktree_path) else {
            continue;
        };
        let common_git_dir = common_git_dir(&git_dir);
        let repository = RepositoryRef {
            common_git_dir: common_git_dir.clone(),
            root: infer_repository_root(worktree_path, &git_dir, &common_git_dir),
        };
        let (branch, head) = read_head(&git_dir, &common_git_dir);
        return Some(WorktreeRef {
            repository,
            git_dir,
            path: worktree_path.to_path_buf(),
            branch,
            head,
            created_by: None,
            managed_by_bootty: false,
        });
    }
    None
}

fn git_directory_from_marker(marker: &Path, worktree_path: &Path) -> Option<PathBuf> {
    if marker.is_dir() {
        return fs::canonicalize(marker).ok();
    }
    if !marker.is_file() {
        return None;
    }

    let contents = fs::read_to_string(marker).ok()?;
    let target = contents
        .lines()
        .next()?
        .trim()
        .strip_prefix("gitdir:")?
        .trim();
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        worktree_path.join(target)
    };
    let target = fs::canonicalize(target).ok()?;
    target.is_dir().then_some(target)
}

fn common_git_dir(git_dir: &Path) -> PathBuf {
    let pointer = git_dir.join("commondir");
    let Some(contents) = fs::read_to_string(pointer).ok() else {
        return git_dir.to_path_buf();
    };
    let target = Path::new(contents.trim());
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        git_dir.join(target)
    };
    fs::canonicalize(target).unwrap_or_else(|_| git_dir.to_path_buf())
}

fn infer_repository_root(
    worktree_path: &Path,
    git_dir: &Path,
    common_git_dir: &Path,
) -> Option<PathBuf> {
    if git_dir == common_git_dir {
        return Some(worktree_path.to_path_buf());
    }
    let candidate = common_git_dir.parent()?.to_path_buf();
    (fs::canonicalize(candidate.join(".git")).ok().as_deref() == Some(common_git_dir))
        .then_some(candidate)
}

fn read_head(git_dir: &Path, common_git_dir: &Path) -> (Option<String>, Option<String>) {
    let Ok(contents) = fs::read_to_string(git_dir.join("HEAD")) else {
        return (None, None);
    };
    let value = contents.trim();
    let Some(reference) = value.strip_prefix("ref:").map(str::trim) else {
        return if value.is_empty() {
            (None, None)
        } else {
            (None, Some(value.to_owned()))
        };
    };
    let branch = reference
        .strip_prefix("refs/heads/")
        .map(std::borrow::ToOwned::to_owned);
    let head = read_git_ref(git_dir, common_git_dir, reference);
    (branch, head)
}

fn read_git_ref(git_dir: &Path, common_git_dir: &Path, reference: &str) -> Option<String> {
    for directory in [git_dir, common_git_dir] {
        if let Ok(value) = fs::read_to_string(directory.join(reference)) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    for directory in [git_dir, common_git_dir] {
        let Ok(packed) = fs::read_to_string(directory.join("packed-refs")) else {
            continue;
        };
        for line in packed.lines() {
            let Some((head, name)) = line.split_once(' ') else {
                continue;
            };
            if name == reference && !head.is_empty() && !head.starts_with('^') {
                return Some(head.to_owned());
            }
        }
    }
    None
}

fn snapshot_file_name(owner: &ClaimOwner, revision: u64) -> String {
    format!(
        "claims-{}-{}-{}-{revision}.json",
        owner.pid,
        owner.started_at_ms,
        encode_component(&owner.instance_id)
    )
}

fn encode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn unique_suffix() -> String {
    let sequence = UNIQUE_SUFFIX.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}-{now}-{sequence}", std::process::id())
}

fn json_to_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn write_owner_only_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = open_owner_only_new(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    set_owner_only_file(path)
}

#[cfg(unix)]
fn sync_snapshot_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_snapshot_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn open_owner_only_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn set_owner_only_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, sync::Arc};

    use tempfile::tempdir;

    use super::*;

    struct NeverDead;

    impl OwnerLiveness for NeverDead {
        fn is_dead(&self, _owner: &ClaimOwner) -> bool {
            false
        }
    }

    struct SelectedDead {
        instance_id: String,
    }

    impl OwnerLiveness for SelectedDead {
        fn is_dead(&self, owner: &ClaimOwner) -> bool {
            owner.instance_id == self.instance_id
        }
    }

    fn owner(instance_id: &str, pid: u32) -> ClaimOwner {
        ClaimOwner {
            instance_id: instance_id.to_owned(),
            pid,
            started_at_ms: 1,
        }
    }

    fn claims(root: &Path, owner: ClaimOwner) -> DirectoryClaims {
        DirectoryClaims::at_with_liveness(root, owner, Arc::new(NeverDead)).unwrap()
    }

    fn claimant(instance_id: &str, session_id: &str, terminal_id: &str) -> ClaimantRef {
        let window = WindowRef {
            instance: InstanceRef {
                instance_id: instance_id.to_owned(),
                generation: 1,
            },
            window_id: "window".to_owned(),
        };
        claimant_for_window(&window, session_id, terminal_id)
    }

    fn claimant_for_window(window: &WindowRef, session_id: &str, terminal_id: &str) -> ClaimantRef {
        let binding = BindingRef {
            window: window.clone(),
            space_id: "space".to_owned(),
            binding_id: "binding".to_owned(),
            generation: 1,
        };
        ClaimantRef {
            session: SessionRef {
                binding: binding.clone(),
                session_id: session_id.to_owned(),
            },
            pane: PaneRef {
                binding: binding.clone(),
                pane_id: format!("pane-{terminal_id}"),
            },
            terminal: TerminalRef {
                binding,
                terminal_id: terminal_id.to_owned(),
                occupant_generation: 1,
            },
        }
    }

    fn directory(path: PathBuf) -> DirectoryRef {
        DirectoryRef {
            canonical_path: path,
            repository: None,
            worktree: None,
        }
    }

    fn linked_directory(root: &Path, name: &str) -> (DirectoryRef, WorktreeRef) {
        let repository = RepositoryRef {
            common_git_dir: root.join("repo/.git"),
            root: Some(root.join("repo")),
        };
        let worktree = WorktreeRef {
            repository: repository.clone(),
            git_dir: root.join(format!("repo/.git/worktrees/{name}")),
            path: root.join(name),
            branch: Some(name.to_owned()),
            head: Some("deadbeef".to_owned()),
            created_by: None,
            managed_by_bootty: false,
        };
        let directory = DirectoryRef {
            canonical_path: worktree.path.join("src"),
            repository: Some(repository),
            worktree: Some(worktree.clone()),
        };
        (directory, worktree)
    }

    #[test]
    fn path_resolution_keeps_a_nonexistent_tail_and_detects_linked_worktrees() {
        let temporary = tempdir().unwrap();
        let root = canonicalize_local_path(temporary.path()).unwrap();
        let repository = root.join("repository");
        let common_git_dir = repository.join(".git");
        fs::create_dir_all(common_git_dir.join("refs/heads")).unwrap();
        fs::write(common_git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(common_git_dir.join("refs/heads/main"), "main-head\n").unwrap();

        let linked = root.join("linked");
        let linked_git_dir = common_git_dir.join("worktrees/linked");
        fs::create_dir_all(&linked_git_dir).unwrap();
        fs::write(linked_git_dir.join("commondir"), "../..\n").unwrap();
        fs::write(linked_git_dir.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
        fs::write(common_git_dir.join("refs/heads/topic"), "topic-head\n").unwrap();
        fs::create_dir_all(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", linked_git_dir.display()),
        )
        .unwrap();

        let unresolved = linked.join("missing/child");
        let resolved = DirectoryRef::resolve(&unresolved).unwrap();
        assert_eq!(resolved.canonical_path, unresolved);
        let alternate = DirectoryRef::resolve(linked.join("missing/./child")).unwrap();
        assert_eq!(resolved.canonical_path, alternate.canonical_path);
        #[cfg(unix)]
        {
            let alias = root.join("linked-alias");
            std::os::unix::fs::symlink(&linked, &alias).unwrap();
            let via_alias = DirectoryRef::resolve(alias.join("missing/child")).unwrap();
            assert_eq!(resolved.canonical_path, via_alias.canonical_path);
        }
        let worktree = resolved.worktree.unwrap();
        assert_eq!(worktree.path, linked);
        assert_eq!(worktree.git_dir, linked_git_dir);
        assert_eq!(worktree.repository.common_git_dir, common_git_dir);
        assert_eq!(worktree.repository.root, Some(repository));
        assert_eq!(worktree.branch.as_deref(), Some("topic"));
        assert_eq!(worktree.head.as_deref(), Some("topic-head"));
        assert!(worktree.is_linked());
    }

    #[test]
    fn launch_claims_are_many_to_many_and_immutable() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let project_directory = directory(temporary.path().join("project"));
        let first = claimant("one", "session-a", "terminal-a");
        let second = claimant("one", "session-b", "terminal-b");

        assert_eq!(
            claims
                .record_launch(first.clone(), project_directory.clone())
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            claims
                .record_launch(second, project_directory.clone())
                .unwrap()
                .revision,
            2
        );
        assert_eq!(claims.claims_for(&project_directory).unwrap().len(), 2);
        assert!(matches!(
            claims.record_launch(first, directory(temporary.path().join("other"))),
            Err(DirectoryClaimsError::ImmutableLaunchIntent { .. })
        ));
    }

    #[test]
    fn launch_identity_ignores_mutable_git_observations() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let (directory, _) = linked_directory(temporary.path(), "linked");
        let terminal = claimant("one", "session", "terminal");

        assert_eq!(
            claims
                .record_launch(terminal.clone(), directory.clone())
                .unwrap()
                .revision,
            1
        );
        let mut refreshed = directory;
        refreshed.worktree.as_mut().unwrap().branch = Some("other-branch".to_owned());
        refreshed.worktree.as_mut().unwrap().head = Some("other-head".to_owned());
        assert_eq!(
            claims.record_launch(terminal, refreshed).unwrap().revision,
            1
        );
    }

    #[test]
    fn observed_cwd_replaces_only_the_observed_claim() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let launch_directory = directory(temporary.path().join("launch"));
        let observed_directory = directory(temporary.path().join("observed"));
        let next_directory = directory(temporary.path().join("next"));
        let terminal = claimant("one", "session", "terminal");

        claims
            .record_launch(terminal.clone(), launch_directory.clone())
            .unwrap();
        claims
            .observe_cwd(terminal.clone(), observed_directory)
            .unwrap();
        let update = claims
            .observe_cwd(terminal, next_directory.clone())
            .unwrap();
        assert_eq!(update.revision, 3);

        let snapshot = claims.snapshot().unwrap();
        assert!(snapshot.claims.iter().any(|claim| {
            claim.source == DirectoryClaimSource::Launch && claim.directory == launch_directory
        }));
        assert!(snapshot.claims.iter().any(|claim| {
            claim.source == DirectoryClaimSource::Observed && claim.directory == next_directory
        }));
        assert_eq!(
            snapshot
                .claims
                .iter()
                .filter(|claim| claim.source == DirectoryClaimSource::Observed)
                .count(),
            1
        );
    }

    #[test]
    fn sharing_warning_uses_documented_precedence() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let ordinary = directory(temporary.path().join("ordinary"));
        let same_session = claimant("one", "session-a", "terminal-a");
        let same_session_second = claimant("one", "session-a", "terminal-b");
        let other_session = claimant("one", "session-b", "terminal-c");

        claims
            .record_launch(same_session.clone(), ordinary.clone())
            .unwrap();
        assert_eq!(
            claims
                .sharing_warning(&same_session_second, &ordinary)
                .unwrap()
                .unwrap()
                .severity,
            DirectoryClaimSeverity::Informational
        );
        assert_eq!(
            claims
                .sharing_warning(&other_session, &ordinary)
                .unwrap()
                .unwrap()
                .severity,
            DirectoryClaimSeverity::Warning
        );

        let (linked, _) = linked_directory(temporary.path(), "linked");
        claims
            .record_launch(same_session_second, linked.clone())
            .unwrap();
        assert_eq!(
            claims
                .sharing_warning(&other_session, &linked)
                .unwrap()
                .unwrap()
                .severity,
            DirectoryClaimSeverity::StrongWarning
        );
    }

    #[test]
    fn stale_snapshot_is_removed_only_after_liveness_proves_death() {
        let temporary = tempdir().unwrap();
        let stale_owner = owner("stale", 100);
        let stale = claims(temporary.path(), stale_owner.clone());
        stale
            .record_launch(
                claimant("stale", "session", "terminal"),
                directory(temporary.path().join("project")),
            )
            .unwrap();
        let stale_path = stale
            .inner
            .snapshots
            .snapshot_path(stale.snapshot().unwrap().revision);
        assert!(stale_path.exists());

        let cleaner = DirectoryClaims::at_with_liveness(
            temporary.path(),
            owner("live", 200),
            Arc::new(SelectedDead {
                instance_id: stale_owner.instance_id,
            }),
        )
        .unwrap();
        assert!(cleaner.cleanup_stale_snapshots().unwrap() >= 1);
        assert!(!stale_path.exists());
    }

    #[test]
    fn untrusted_snapshot_blocks_worktree_removal_recheck() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let (_, worktree) = linked_directory(temporary.path(), "linked");
        fs::write(temporary.path().join("untrusted.json"), b"not JSON").unwrap();

        assert!(matches!(
            claims.assess_worktree_removal(&worktree, None),
            Err(DirectoryClaimsError::UntrustedSnapshot { .. })
        ));
    }

    #[test]
    fn final_recheck_blocks_a_claim_published_after_preflight() {
        let temporary = tempdir().unwrap();
        let first = claims(temporary.path(), owner("first", 100));
        let second = claims(temporary.path(), owner("second", 200));
        let (directory, worktree) = linked_directory(temporary.path(), "linked");
        let requester = claimant("first", "session-a", "terminal-a");
        let other = claimant("second", "session-b", "terminal-b");
        let request = WorktreeRemovalRequest {
            requester_session: Some(requester.session.clone()),
            confirmation: None,
        };

        let preflight = first
            .assess_worktree_removal(&worktree, request.requester_session.as_ref())
            .unwrap();
        assert!(preflight.active_claims.is_empty());

        second.record_launch(other, directory).unwrap();
        let invoked = Cell::new(false);
        let result = first.remove_worktree(&worktree, &request, |_| {
            invoked.set(true);
            Ok(())
        });
        assert!(matches!(
            result,
            Err(DirectoryClaimsError::ConfirmationRequired { .. })
        ));
        assert!(!invoked.get());

        let assessment = first
            .assess_worktree_removal(&worktree, request.requester_session.as_ref())
            .unwrap();
        let confirmation = assessment.bound_confirmation().unwrap();
        let stale_request = WorktreeRemovalRequest {
            requester_session: request.requester_session.clone(),
            confirmation: Some(WorktreeRemovalConfirmation {
                worktree: worktree.clone(),
                conflicting_claims: Vec::new(),
            }),
        };
        assert!(matches!(
            first.remove_worktree(&worktree, &stale_request, |_| Ok(())),
            Err(DirectoryClaimsError::StaleConfirmation { .. })
        ));

        let confirmed_request = WorktreeRemovalRequest {
            requester_session: request.requester_session,
            confirmation: Some(confirmation),
        };
        let allowed = Cell::new(false);
        first
            .remove_worktree(&worktree, &confirmed_request, |_| {
                allowed.set(true);
                Ok(())
            })
            .unwrap();
        assert!(allowed.get());
    }

    #[test]
    fn close_releases_all_claims_for_only_the_removed_occupant() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let launch = directory(temporary.path().join("launch"));
        let observed = directory(temporary.path().join("observed"));
        let replacement_observed = directory(temporary.path().join("replacement-observed"));
        let removed = claimant("one", "session", "terminal");
        let replacement = ClaimantRef {
            terminal: TerminalRef {
                occupant_generation: 2,
                ..removed.terminal.clone()
            },
            ..removed.clone()
        };

        claims.record_launch(removed.clone(), launch).unwrap();
        claims.observe_cwd(removed.clone(), observed).unwrap();
        claims
            .observe_cwd(replacement.clone(), replacement_observed.clone())
            .unwrap();
        claims.release_claimant(&removed).unwrap();

        let snapshot = claims.snapshot().unwrap();
        assert!(
            snapshot
                .claims
                .iter()
                .all(|claim| claim.terminal != removed.terminal)
        );
        assert!(snapshot.claims.iter().any(|claim| {
            claim.terminal == replacement.terminal
                && claim.source == DirectoryClaimSource::Observed
                && claim.directory == replacement_observed
        }));
    }

    #[test]
    fn closing_window_releases_exact_instance_window_claims_in_one_revision() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let closing_window = WindowRef {
            instance: InstanceRef {
                instance_id: "instance".to_owned(),
                generation: 1,
            },
            window_id: "closing".to_owned(),
        };
        let sibling_window = WindowRef {
            instance: closing_window.instance.clone(),
            window_id: "sibling".to_owned(),
        };
        let replacement_window = WindowRef {
            instance: InstanceRef {
                instance_id: closing_window.instance.instance_id.clone(),
                generation: 2,
            },
            window_id: closing_window.window_id.clone(),
        };
        let closing = claimant_for_window(&closing_window, "closing", "closing-terminal");
        let sibling = claimant_for_window(&sibling_window, "sibling", "sibling-terminal");
        let replacement =
            claimant_for_window(&replacement_window, "replacement", "replacement-terminal");
        let (closed_directory, closed_worktree) = linked_directory(temporary.path(), "closed");

        claims
            .record_launch(closing.clone(), closed_directory.clone())
            .unwrap();
        claims.observe_cwd(closing, closed_directory).unwrap();
        claims
            .record_launch(sibling.clone(), directory(temporary.path().join("sibling")))
            .unwrap();
        claims
            .record_launch(
                replacement.clone(),
                directory(temporary.path().join("replacement")),
            )
            .unwrap();
        let before = claims.snapshot().unwrap();

        assert_eq!(
            claims.release_window_claims(&closing_window).unwrap(),
            Some(before.revision + 1)
        );

        let snapshot = claims.snapshot().unwrap();
        assert_eq!(snapshot.revision, before.revision + 1);
        assert!(
            snapshot
                .claims
                .iter()
                .all(|claim| claim.terminal.binding.window != closing_window)
        );
        assert!(
            snapshot
                .claims
                .iter()
                .any(|claim| claim.terminal == sibling.terminal)
        );
        assert!(
            snapshot
                .claims
                .iter()
                .any(|claim| claim.terminal == replacement.terminal)
        );
        assert!(
            claims
                .assess_worktree_removal(&closed_worktree, None)
                .unwrap()
                .active_claims
                .is_empty()
        );
        assert_eq!(claims.release_window_claims(&closing_window).unwrap(), None);
    }

    #[test]
    fn replacement_is_self_equivalent_but_cross_session_worktree_warns() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let (linked, _) = linked_directory(temporary.path(), "linked");
        let prior = claimant("one", "session-a", "terminal-a");
        let replacement = ClaimantRef {
            terminal: TerminalRef {
                occupant_generation: 2,
                ..prior.terminal.clone()
            },
            ..prior.clone()
        };
        let other_session = ClaimantRef {
            session: SessionRef {
                binding: prior.session.binding.clone(),
                session_id: "session-b".to_owned(),
            },
            pane: prior.pane.clone(),
            terminal: TerminalRef {
                occupant_generation: 3,
                ..prior.terminal.clone()
            },
        };

        claims.record_launch(prior.clone(), linked.clone()).unwrap();
        claims
            .observe_cwd(
                prior.clone(),
                directory(temporary.path().join("prior-observed")),
            )
            .unwrap();
        claims.release_observed_claimant(&prior).unwrap();
        let snapshot = claims.snapshot().unwrap();
        assert!(snapshot.claims.iter().any(|claim| {
            claim.terminal == prior.terminal && claim.source == DirectoryClaimSource::Launch
        }));
        assert!(snapshot.claims.iter().all(|claim| {
            !(claim.terminal == prior.terminal && claim.source == DirectoryClaimSource::Observed)
        }));
        assert_eq!(
            claims
                .observe_cwd(replacement, linked.clone())
                .unwrap()
                .warning,
            None
        );
        assert_eq!(
            claims
                .sharing_warning(&other_session, &linked)
                .unwrap()
                .unwrap()
                .severity,
            DirectoryClaimSeverity::StrongWarning
        );
    }

    #[test]
    fn topology_reconciliation_recovers_a_dropped_close_without_erasing_live_launch_intent() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let closed = claimant("one", "closed-session", "closed-terminal");
        let prior_live = claimant("one", "live-session", "live-terminal");
        let replacement_live = ClaimantRef {
            terminal: TerminalRef {
                occupant_generation: 2,
                ..prior_live.terminal.clone()
            },
            ..prior_live.clone()
        };

        claims
            .record_launch(
                closed.clone(),
                directory(temporary.path().join("closed-launch")),
            )
            .unwrap();
        claims
            .observe_cwd(
                closed.clone(),
                directory(temporary.path().join("closed-observed")),
            )
            .unwrap();
        claims
            .record_launch(
                prior_live.clone(),
                directory(temporary.path().join("live-launch")),
            )
            .unwrap();
        claims
            .observe_cwd(
                prior_live.clone(),
                directory(temporary.path().join("prior-live-observed")),
            )
            .unwrap();

        assert!(
            claims
                .reconcile_live_claimants(
                    &replacement_live.terminal.binding,
                    [replacement_live.clone()],
                )
                .unwrap()
                .is_some()
        );
        let snapshot = claims.snapshot().unwrap();
        assert!(
            snapshot
                .claims
                .iter()
                .all(|claim| claim.terminal != closed.terminal)
        );
        assert!(snapshot.claims.iter().any(|claim| {
            claim.terminal == prior_live.terminal && claim.source == DirectoryClaimSource::Launch
        }));
        assert!(snapshot.claims.iter().all(|claim| {
            !(claim.terminal == prior_live.terminal
                && claim.source == DirectoryClaimSource::Observed)
        }));
    }

    #[test]
    fn retired_binding_generation_rebases_live_launch_and_drops_stale_claims_atomically() {
        let temporary = tempdir().unwrap();
        let claims = claims(temporary.path(), owner("one", 100));
        let retired_live = claimant("one", "live-session", "live-terminal");
        let current_binding = BindingRef {
            generation: 2,
            ..retired_live.terminal.binding.clone()
        };
        let current_live = ClaimantRef {
            session: SessionRef {
                binding: current_binding.clone(),
                session_id: retired_live.session.session_id.clone(),
            },
            pane: PaneRef {
                binding: current_binding.clone(),
                pane_id: retired_live.pane.pane_id.clone(),
            },
            terminal: TerminalRef {
                binding: current_binding.clone(),
                terminal_id: retired_live.terminal.terminal_id.clone(),
                occupant_generation: 2,
            },
        };
        let retired_closed = claimant("one", "closed-session", "closed-terminal");
        let launch_directory = directory(temporary.path().join("live-launch"));

        claims
            .record_launch(retired_live.clone(), launch_directory.clone())
            .unwrap();
        claims
            .observe_cwd(
                retired_live.clone(),
                directory(temporary.path().join("live-observed")),
            )
            .unwrap();
        claims
            .record_launch(
                retired_closed.clone(),
                directory(temporary.path().join("closed-launch")),
            )
            .unwrap();
        claims
            .observe_cwd(
                retired_closed.clone(),
                directory(temporary.path().join("closed-observed")),
            )
            .unwrap();

        assert_eq!(
            claims
                .rebase_retired_binding_claims(&current_binding, [current_live.clone()])
                .unwrap(),
            Some(5)
        );
        let snapshot = claims.snapshot().unwrap();
        assert_eq!(snapshot.revision, 5);
        assert_eq!(snapshot.claims.len(), 1);
        let claim = &snapshot.claims[0];
        assert_eq!(claim.directory, launch_directory);
        assert_eq!(claim.source, DirectoryClaimSource::Launch);
        assert_eq!(claim.session, current_live.session);
        assert_eq!(claim.pane, current_live.pane);
        assert_eq!(claim.terminal, current_live.terminal);
        assert_eq!(claim.since_revision, 1);
        assert!(
            snapshot
                .claims
                .iter()
                .all(|claim| claim.source != DirectoryClaimSource::Observed)
        );
        assert!(snapshot.claims.iter().all(|claim| {
            claim.terminal != retired_live.terminal && claim.terminal != retired_closed.terminal
        }));
    }

    #[test]
    fn live_snapshots_include_claims_from_every_live_owner() {
        let temporary = tempdir().unwrap();
        let first = claims(temporary.path(), owner("first", 100));
        let second = claims(temporary.path(), owner("second", 200));
        first
            .record_launch(
                claimant("first", "first-session", "first-terminal"),
                directory(temporary.path().join("first")),
            )
            .unwrap();
        second
            .record_launch(
                claimant("second", "second-session", "second-terminal"),
                directory(temporary.path().join("second")),
            )
            .unwrap();

        let snapshots = first.live_snapshots().unwrap();
        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.owner.instance_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(
            snapshots
                .iter()
                .flat_map(|snapshot| snapshot.claims.iter())
                .map(|claim| claim.session.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first-session", "second-session"]
        );
    }
}
