use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rmux_ipc::{LocalEndpoint, endpoint_for_label};
use serde::{Deserialize, Serialize};

use bootty_identity::ApplicationIdentity;

use crate::protocol::PROTOCOL_VERSION;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDescriptor {
    pub instance_id: String,
    pub generation: u64,
    pub pid: u32,
    pub window_state_key: String,
    pub endpoint: PathBuf,
    pub started_at_ms: u128,
    pub protocol_version: u32,
}

pub(crate) struct ControlInstanceLease {
    descriptor: InstanceDescriptor,
    descriptor_path: PathBuf,
    claim_lock: Option<File>,
}

impl ControlInstanceLease {
    pub(crate) fn claim(window_state_key: &str) -> Result<Self> {
        let directory = prepare_instance_directory()?;
        let claim_lock = lock_instance(&directory)?;
        if Self::discover_locked(&directory)?.is_some() {
            anyhow::bail!(
                "{} is already running",
                ApplicationIdentity::current().display_name()
            );
        }

        let generation = generation_token()?;
        let endpoint = endpoint_for_generation(generation)?;
        prepare_endpoint_parent(&endpoint)?;
        let started_at_ms = current_process_started_at_ms()?;
        let descriptor = InstanceDescriptor {
            instance_id: ApplicationIdentity::current().cli_name().to_owned(),
            generation,
            pid: std::process::id(),
            window_state_key: window_state_key.to_owned(),
            endpoint: endpoint.into_path(),
            started_at_ms,
            protocol_version: PROTOCOL_VERSION,
        };

        Ok(Self {
            descriptor,
            descriptor_path: directory.join("control.json"),
            claim_lock: Some(claim_lock),
        })
    }

    pub(crate) fn descriptor(&self) -> &InstanceDescriptor {
        &self.descriptor
    }

    pub(crate) fn publish(&mut self) -> Result<()> {
        let directory = self
            .descriptor_path
            .parent()
            .context("control descriptor has no parent directory")?;
        let temporary = directory.join(format!(
            ".control-{:016x}.json.tmp",
            self.descriptor.generation
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .context("create temporary control descriptor")?;
            serde_json::to_writer(&mut file, &self.descriptor)?;
            file.flush()?;
            file.sync_all()?;
            set_owner_only_file(&temporary)?;
            fs::rename(&temporary, &self.descriptor_path).context("publish control descriptor")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        self.claim_lock.take();
        result
    }

    pub(crate) fn discover() -> Result<Option<InstanceDescriptor>> {
        let directory = prepare_instance_directory()?;
        let _claim_lock = lock_instance(&directory)?;
        Self::discover_locked(&directory)
    }

    fn discover_locked(directory: &Path) -> Result<Option<InstanceDescriptor>> {
        let descriptor_path = directory.join("control.json");
        let bytes = match fs::read(&descriptor_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("read control descriptor"),
        };
        let Ok(descriptor) = serde_json::from_slice::<InstanceDescriptor>(&bytes) else {
            remove_descriptor_if_matches(&descriptor_path, &bytes);
            return Ok(None);
        };
        let expected_endpoint = endpoint_for_generation(descriptor.generation)?.into_path();
        let valid_namespace = descriptor.protocol_version == PROTOCOL_VERSION
            && descriptor.instance_id == ApplicationIdentity::current().cli_name()
            && descriptor.endpoint == expected_endpoint;
        if !valid_namespace {
            remove_descriptor_if_matches(&descriptor_path, &bytes);
            return Ok(None);
        }
        if instance_process_is_dead(&descriptor) {
            remove_descriptor_if_matches(&descriptor_path, &bytes);
            remove_endpoint(&descriptor.endpoint);
            return Ok(None);
        }
        Ok(Some(descriptor))
    }

    pub(crate) fn abort(self) {
        self.release();
    }

    pub(crate) fn release(self) {
        drop(self);
    }

    fn cleanup(&mut self) {
        let claim_lock = self.claim_lock.take().or_else(|| {
            self.descriptor_path
                .parent()
                .and_then(|directory| lock_instance(directory).ok())
        });
        let Some(_claim_lock) = claim_lock else {
            return;
        };
        if let Ok(expected) = serde_json::to_vec(&self.descriptor) {
            remove_descriptor_if_matches(&self.descriptor_path, &expected);
        }
        remove_endpoint(&self.descriptor.endpoint);
    }
}

impl Drop for ControlInstanceLease {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn prepare_instance_directory() -> Result<PathBuf> {
    let directory = instance_directory()?;
    fs::create_dir_all(&directory)?;
    set_owner_only_directory(&directory)?;
    Ok(directory)
}

fn lock_instance(directory: &Path) -> Result<File> {
    let path = directory.join("control.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .context("open control instance lease")?;
    set_owner_only_file(&path)?;
    file.lock().context("lock control instance lease")?;
    Ok(file)
}

fn generation_token() -> Result<u64> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("generate control instance generation: {error}"))?;
    let generation = u64::from_le_bytes(bytes);
    if generation == 0 {
        return generation_token();
    }
    Ok(generation)
}

fn current_process_started_at_ms() -> Result<u128> {
    process_started_at_ms(std::process::id())
        .context("current process is missing from the process table")
}

fn endpoint_for_generation(generation: u64) -> Result<LocalEndpoint> {
    let identity = ApplicationIdentity::current();
    let prefix = if identity == ApplicationIdentity::Production {
        'b'
    } else {
        'd'
    };
    Ok(endpoint_for_label(format!("{prefix}{generation:016x}"))?)
}

fn prepare_endpoint_parent(endpoint: &LocalEndpoint) -> Result<()> {
    if let Some(parent) = endpoint.as_path().parent() {
        fs::create_dir_all(parent)?;
        set_owner_only_directory(parent)?;
    }
    Ok(())
}

fn remove_descriptor_if_matches(path: &Path, expected: &[u8]) {
    if fs::read(path).is_ok_and(|current| current == expected) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(unix)]
fn remove_endpoint(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(windows)]
fn remove_endpoint(_path: &Path) {}

fn instance_directory() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("no user-private runtime directory is available")?;
    Ok(base.join(ApplicationIdentity::current().cli_name()))
}

fn instance_process_is_dead(instance: &InstanceDescriptor) -> bool {
    process_started_at_ms(instance.pid) != Some(instance.started_at_ms)
}

fn process_started_at_ms(pid: u32) -> Option<u128> {
    let system = sysinfo::System::new_all();
    system
        .process(sysinfo::Pid::from_u32(pid))
        .map(|process| u128::from(process.start_time()) * 1000)
}

#[cfg(unix)]
pub(crate) fn same_user(peer: &rmux_ipc::PeerIdentity) -> bool {
    peer.uid == rmux_os::identity::real_user_id()
}

#[cfg(windows)]
pub(crate) fn same_user(peer: &rmux_ipc::PeerIdentity) -> bool {
    rmux_os::identity::IdentityResolver::current().is_ok_and(|identity| identity == peer.user)
}

#[cfg(unix)]
pub(crate) fn set_owner_only_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
pub(crate) fn set_owner_only_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
pub(crate) fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Ok(())
}
