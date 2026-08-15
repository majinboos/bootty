use anyhow::{Context, Result};
use base64::engine::general_purpose;
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_SHARED_MEMORY_FIXTURE: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct SharedMemoryFixture {
    name: CString,
}

impl SharedMemoryFixture {
    pub(crate) fn write(bytes: &[u8]) -> Result<Self> {
        let sequence = NEXT_SHARED_MEMORY_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("/bt{:x}{sequence:x}", std::process::id()))?;
        unsafe { libc::shm_unlink(name.as_ptr()) };
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("create shared-memory fixture");
        }

        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let fd = file.as_raw_fd();
        if unsafe { libc::ftruncate(fd, bytes.len() as libc::off_t) } != 0 {
            return Err(std::io::Error::last_os_error()).context("size shared-memory fixture");
        }
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes.len(),
                libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("map shared-memory fixture");
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapping.cast::<u8>(), bytes.len());
            if libc::munmap(mapping, bytes.len()) != 0 {
                return Err(std::io::Error::last_os_error()).context("unmap shared-memory fixture");
            }
        }
        drop(file);
        Ok(Self { name })
    }

    pub(crate) fn payload(&self) -> Result<String> {
        Ok(base64::Engine::encode(
            &general_purpose::STANDARD,
            self.name
                .to_str()
                .context("shared-memory name is not UTF-8")?
                .as_bytes(),
        ))
    }
}

impl Drop for SharedMemoryFixture {
    fn drop(&mut self) {
        unsafe {
            libc::shm_unlink(self.name.as_ptr());
        }
    }
}

pub(crate) fn is_shared_memory_unavailable(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>().is_some_and(|io| {
        matches!(
            io.raw_os_error(),
            Some(code) if code == libc::ENXIO || code == libc::ENOSYS || code == libc::ENODEV
        )
    })
}
