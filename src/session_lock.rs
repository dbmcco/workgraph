//! Per-session handler lock — enforces at-most-one live handler per
//! chat session at a time.
//!
//! This module implements the lock contract:
//!
//!   * `acquire(dir, kind)`: O_EXCL create on `<dir>/.handler.pid`.
//!     If the file exists with a LIVE PID, refuses. If the file
//!     exists with a DEAD PID, recovers (removes + retakes).
//!   * `SessionLock::drop`: removes the file on clean exit.
//!   * `read_holder(dir)`: non-destructive read of who currently
//!     holds the lock.
//!   * `request_release(dir)`: writes `<dir>/.handler.release-requested`
//!     as a cooperative signal.
//!
//! The lock file format is deliberately small and human-readable:
//!
//! ```text
//! <pid>\n
//! <iso-8601-start-time>\n
//! <kind-label>\n
//! ```
//!
//! Stale detection uses `kill(pid, 0)` on Unix. On non-Unix targets we
//! conservatively treat the lock as alive.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

const LOCK_FILENAME: &str = ".handler.pid";
const RELEASE_MARKER: &str = ".handler.release-requested";

/// What kind of handler owns the lock. Used for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerKind {
    InteractiveNex,
    AutonomousNex,
    ChatNex,
    TuiPty,
    Adapter,
}

impl HandlerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::InteractiveNex => "interactive-nex",
            Self::AutonomousNex => "autonomous-nex",
            Self::ChatNex => "chat-nex",
            Self::TuiPty => "tui-pty",
            Self::Adapter => "adapter",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "interactive-nex" => Some(Self::InteractiveNex),
            "autonomous-nex" => Some(Self::AutonomousNex),
            "chat-nex" => Some(Self::ChatNex),
            "tui-pty" => Some(Self::TuiPty),
            "adapter" => Some(Self::Adapter),
            _ => None,
        }
    }
}

/// Snapshot of the current lock holder.
#[derive(Clone, Debug)]
pub struct LockInfo {
    pub pid: u32,
    pub started_at: String,
    pub kind: Option<HandlerKind>,
    pub alive: bool,
}

/// RAII lock handle. Drop removes the file.
pub struct SessionLock {
    path: PathBuf,
    released: bool,
}

impl SessionLock {
    pub fn lock_path(chat_dir: &Path) -> PathBuf {
        chat_dir.join(LOCK_FILENAME)
    }

    pub fn release_marker_path(chat_dir: &Path) -> PathBuf {
        chat_dir.join(RELEASE_MARKER)
    }

    pub fn acquire(chat_dir: &Path, kind: HandlerKind) -> Result<Self> {
        std::fs::create_dir_all(chat_dir)
            .with_context(|| format!("create chat dir {:?}", chat_dir))?;
        let path = Self::lock_path(chat_dir);

        let create_result = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path);

        match create_result {
            Ok(mut file) => {
                let contents = format!(
                    "{}\n{}\n{}\n",
                    std::process::id(),
                    chrono::Utc::now().to_rfc3339(),
                    kind.label(),
                );
                file.write_all(contents.as_bytes())
                    .with_context(|| format!("write lock file {:?}", path))?;
                file.sync_all().context("fsync lock file")?;
                Ok(Self {
                    path,
                    released: false,
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                match read_holder_at(&path)? {
                    Some(holder) if holder.alive => Err(anyhow!(
                        "session lock held by live handler pid={} kind={} started={}",
                        holder.pid,
                        holder.kind.map(|k| k.label()).unwrap_or("unknown"),
                        holder.started_at
                    )),
                    Some(holder) => {
                        eprintln!(
                            "[session-lock] recovering stale lock (dead pid={}, kind={}) at {:?}",
                            holder.pid,
                            holder.kind.map(|k| k.label()).unwrap_or("unknown"),
                            path
                        );
                        std::fs::remove_file(&path)
                            .with_context(|| format!("remove stale lock {:?}", path))?;
                        Self::acquire(chat_dir, kind)
                    }
                    None => {
                        eprintln!("[session-lock] recovering unparseable lock at {:?}", path);
                        std::fs::remove_file(&path)
                            .with_context(|| format!("remove corrupt lock {:?}", path))?;
                        Self::acquire(chat_dir, kind)
                    }
                }
            }
            Err(err) => Err(anyhow!("open lock file {:?}: {}", path, err)),
        }
    }

    pub fn release(&mut self) {
        if self.released {
            return;
        }
        if self.path.exists()
            && let Err(err) = std::fs::remove_file(&self.path)
        {
            eprintln!(
                "[session-lock] warning: failed to remove lock {:?}: {}",
                self.path, err
            );
        }
        self.released = true;
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        self.release();
    }
}

pub fn read_holder(chat_dir: &Path) -> Result<Option<LockInfo>> {
    read_holder_at(&SessionLock::lock_path(chat_dir))
}

fn read_holder_at(path: &Path) -> Result<Option<LockInfo>> {
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(anyhow!("open lock file {:?}: {}", path, err)),
    };

    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .with_context(|| format!("read lock file {:?}", path))?;

    let mut lines = buf.lines();
    let pid_line = match lines.next() {
        Some(line) => line,
        None => return Ok(None),
    };
    let pid: u32 = match pid_line.trim().parse() {
        Ok(pid) => pid,
        Err(_) => return Ok(None),
    };
    let started_at = lines.next().unwrap_or("").to_string();
    let kind = lines.next().and_then(HandlerKind::parse);

    Ok(Some(LockInfo {
        pid,
        started_at,
        kind,
        alive: pid_is_alive(pid),
    }))
}

pub fn request_release(chat_dir: &Path) -> Result<()> {
    let marker = SessionLock::release_marker_path(chat_dir);
    std::fs::write(&marker, format!("{}\n", chrono::Utc::now().to_rfc3339()))
        .with_context(|| format!("write release marker {:?}", marker))?;
    Ok(())
}

pub fn release_requested(chat_dir: &Path) -> bool {
    SessionLock::release_marker_path(chat_dir).exists()
}

pub fn clear_release_marker(chat_dir: &Path) {
    let marker = SessionLock::release_marker_path(chat_dir);
    if marker.exists() {
        let _ = std::fs::remove_file(&marker);
    }
}

pub fn wait_for_release(chat_dir: &Path, timeout: Duration) -> Result<()> {
    let poll = Duration::from_millis(100);
    let start = std::time::Instant::now();
    loop {
        match read_holder(chat_dir)? {
            None => return Ok(()),
            Some(info) if !info.alive => return Ok(()),
            Some(_) => {
                if start.elapsed() >= timeout {
                    return Err(anyhow!(
                        "timed out waiting for lock release after {:?}",
                        timeout
                    ));
                }
                std::thread::sleep(poll);
            }
        }
    }
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        true
    } else {
        let errno = std::io::Error::last_os_error().raw_os_error();
        errno == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquire_creates_lock_file_with_pid() {
        let dir = tempdir().unwrap();
        let lock = SessionLock::acquire(dir.path(), HandlerKind::InteractiveNex).unwrap();
        let path = SessionLock::lock_path(dir.path());
        assert!(path.exists());
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains(&format!("{}", std::process::id())));
        assert!(contents.contains("interactive-nex"));
        drop(lock);
        assert!(!path.exists(), "Drop should remove the lock file");
    }

    #[test]
    fn second_acquire_fails_while_first_held() {
        let dir = tempdir().unwrap();
        let _first = SessionLock::acquire(dir.path(), HandlerKind::InteractiveNex).unwrap();
        let second = SessionLock::acquire(dir.path(), HandlerKind::InteractiveNex);
        assert!(second.is_err(), "second acquire must fail while first held");
        let err = second.err().unwrap().to_string();
        assert!(
            err.contains("pid="),
            "error must name the holder pid: {}",
            err
        );
    }

    #[test]
    fn stale_lock_recovers() {
        let dir = tempdir().unwrap();
        let path = SessionLock::lock_path(dir.path());
        std::fs::write(&path, "999999\n2020-01-01T00:00:00Z\nchat-nex\n").unwrap();
        let lock = SessionLock::acquire(dir.path(), HandlerKind::ChatNex).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains(&format!("{}", std::process::id())));
        drop(lock);
    }

    #[test]
    fn read_holder_returns_live_flag() {
        let dir = tempdir().unwrap();
        let _lock = SessionLock::acquire(dir.path(), HandlerKind::TuiPty).unwrap();
        let info = read_holder(dir.path()).unwrap().unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.kind, Some(HandlerKind::TuiPty));
        assert!(info.alive);
    }

    #[test]
    fn read_holder_none_when_no_lock() {
        let dir = tempdir().unwrap();
        assert!(read_holder(dir.path()).unwrap().is_none());
    }

    #[test]
    fn release_marker_round_trip() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        assert!(!release_requested(dir.path()));
        request_release(dir.path()).unwrap();
        assert!(release_requested(dir.path()));
        clear_release_marker(dir.path());
        assert!(!release_requested(dir.path()));
    }

    #[test]
    fn explicit_release_allows_reacquire() {
        let dir = tempdir().unwrap();
        let mut lock = SessionLock::acquire(dir.path(), HandlerKind::ChatNex).unwrap();
        lock.release();
        let _new = SessionLock::acquire(dir.path(), HandlerKind::ChatNex).unwrap();
    }

    #[test]
    fn corrupt_lock_recovers() {
        let dir = tempdir().unwrap();
        let path = SessionLock::lock_path(dir.path());
        std::fs::write(&path, "not a pid\ngarbage\n").unwrap();
        let lock = SessionLock::acquire(dir.path(), HandlerKind::ChatNex).unwrap();
        drop(lock);
    }

    #[test]
    fn wait_for_release_succeeds_when_free() {
        let dir = tempdir().unwrap();
        wait_for_release(dir.path(), Duration::from_millis(50)).unwrap();
    }

    #[test]
    fn wait_for_release_times_out_while_held() {
        let dir = tempdir().unwrap();
        let _lock = SessionLock::acquire(dir.path(), HandlerKind::ChatNex).unwrap();
        let result = wait_for_release(dir.path(), Duration::from_millis(100));
        assert!(result.is_err());
    }
}
