//! Single-instance guard. Two presence processes would both play into the
//! virtual device, and the call app would hear the sum: the voice doubled
//! (+6 dB) and possibly comb-filtered.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "presence.lock";

/// Held for the life of the process; the OS releases the lock on exit, even
/// after a crash, so there is no stale-lock cleanup.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

#[derive(Debug)]
pub enum LockError {
    /// Another process holds the lock; its pid if it wrote one.
    Held {
        pid: Option<u32>,
    },
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held { pid } => {
                write!(f, "presence is already running")?;
                if let Some(pid) = pid {
                    write!(f, " (pid {pid})")?;
                }
                write!(
                    f,
                    "; a second instance would also play into the virtual device and double \
                     your voice (+6 dB). Quit the other one first."
                )
            }
            LockError::Io(e) => write!(f, "couldn't take the instance lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// The lock lives next to the config file, or in the temp dir without one.
pub fn lock_path(config_path: Option<&Path>) -> PathBuf {
    let dir = match config_path.and_then(Path::parent) {
        Some(dir) if dir.as_os_str().is_empty() => PathBuf::from("."),
        Some(dir) => dir.to_path_buf(),
        None => std::env::temp_dir(),
    };
    dir.join(LOCK_FILE)
}

pub fn acquire(path: &Path) -> Result<InstanceLock, LockError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(LockError::Io)?;
    }
    // No truncate: the current holder's pid must survive a failed attempt.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(LockError::Io)?;
    // SAFETY: flock on a valid, open descriptor owned by `file`.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        return Err(if err.kind() == io::ErrorKind::WouldBlock {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            LockError::Held {
                pid: text.trim().parse().ok(),
            }
        } else {
            LockError::Io(err)
        });
    }
    // Best effort: the pid is only for the message.
    let _ = file
        .set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.flush());
    Ok(InstanceLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_refused_with_pid_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        let first = acquire(&path).unwrap();
        match acquire(&path) {
            Err(LockError::Held { pid }) => assert_eq!(pid, Some(std::process::id())),
            other => panic!("expected Held, got {other:?}"),
        }
        let msg = LockError::Held { pid: Some(42) }.to_string();
        assert!(msg.contains("already running (pid 42)"), "{msg}");
        drop(first);
        acquire(&path).expect("lock is free again after drop");
    }

    #[test]
    fn lock_sits_next_to_the_config() {
        assert_eq!(
            lock_path(Some(Path::new("/tmp/x/config.toml"))),
            Path::new("/tmp/x/presence.lock")
        );
        assert_eq!(lock_path(None), std::env::temp_dir().join(LOCK_FILE));
        assert_eq!(
            lock_path(Some(Path::new("config.toml"))),
            Path::new("./presence.lock")
        );
    }
}
