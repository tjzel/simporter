use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct LockGuard {
    file: File,
}

impl LockGuard {
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Option<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("failed to open lock file {}", path.display()))?;
        let deadline = Instant::now() + timeout;
        loop {
            if try_flock(&file)? {
                return Ok(Some(Self { file }));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn try_flock(file: &File) -> Result<bool> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EWOULDBLOCK | libc::EINTR) => Ok(false),
        _ => Err(error).context("flock failed"),
    }
}

pub fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_in_same_process_blocks_until_timeout() {
        let dir = crate::testing::temp_dir("lock-exclusive");
        let path = dir.join("state.lock");
        let held = LockGuard::acquire(&path, Duration::from_millis(200)).unwrap();
        assert!(held.is_some());
        let contended = LockGuard::acquire(&path, Duration::from_millis(100)).unwrap();
        assert!(contended.is_none());
    }

    #[test]
    fn lock_is_reacquirable_after_drop() {
        let dir = crate::testing::temp_dir("lock-release");
        let path = dir.join("state.lock");
        drop(LockGuard::acquire(&path, Duration::from_millis(200)).unwrap());
        let again = LockGuard::acquire(&path, Duration::from_millis(200)).unwrap();
        assert!(again.is_some());
    }

    #[test]
    fn current_process_is_alive() {
        assert!(process_is_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_treated_as_a_live_owner() {
        assert!(!process_is_alive(u32::MAX));
    }
}
