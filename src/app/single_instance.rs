//! Advisory flock guard: the OS releases the lock automatically if the process
//! crashes, so a crashed instance never blocks a new one.

use std::ffi::CString;
use std::os::unix::io::RawFd;

use anyhow::{bail, Context as _, Result};

const APP_DIR: &str = "budlink-mac";
const LOCK_NAME: &str = "budlink-mac.lock";

pub struct SingleInstance {
    fd: RawFd,
}

impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        let dir = lock_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let path = CString::new(dir.join(LOCK_NAME).to_string_lossy().into_owned())?;

        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("failed to open lock file");
        }

        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                bail!("another budlink-mac menubar instance is already running");
            }
            return Err(err).context("failed to lock instance file");
        }

        // Record the pid for debugging (best-effort).
        let pid = unsafe { libc::getpid() }.to_string();
        unsafe {
            libc::ftruncate(fd, 0);
            libc::write(fd, pid.as_ptr() as *const _, pid.len());
        }

        Ok(Self { fd })
    }
}

fn lock_dir() -> Result<std::path::PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        return Ok(format!("{home}/Library/Application Support/{APP_DIR}").into());
    }
    if let Ok(tmp) = std::env::var("TMPDIR") {
        return Ok(std::path::PathBuf::from(tmp).join(APP_DIR));
    }
    Ok(std::env::temp_dir().join(APP_DIR))
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.fd, libc::LOCK_UN);
            libc::close(self.fd);
        }
    }
}
