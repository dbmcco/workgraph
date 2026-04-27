//! Per-session handler lock — enforces at-most-one live handler per
//! chat session at a time.
//!
//! External contract:
//! - `.handler.pid` remains the human-readable holder metadata file
//! - `.handler.release-requested` remains the cooperative release marker
//!
//! Internally we serialize acquisition through a sidecar guard file held for
//! the lock lifetime, so `.handler.pid` can be updated safely and `release()`
//! only removes metadata that still belongs to this instance.
//!
//! Current implementation status:
//! - Unix: fully supported via `flock()` on the sidecar guard file
//! - non-Unix: compiles cleanly but returns an explicit unsupported error
//!   rather than pretending to provide at-most-one-owner semantics

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

const LOCK_FILENAME: &str = ".handler.pid";
const RELEASE_MARKER: &str = ".handler.release-requested";
const GUARD_FILENAME: &str = ".handler.pid.guard";
const OWNER_METADATA_WAIT: Duration = Duration::from_millis(250);

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

#[derive(Clone, Debug)]
pub struct LockInfo {
    pub pid: u32,
    pub started_at: String,
    pub kind: Option<HandlerKind>,
    pub alive: bool,
}

struct GuardFile {
    file: File,
}

impl GuardFile {
    fn acquire(chat_dir: &Path) -> Result<Option<Self>> {
        let path = guard_path(chat_dir);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .with_context(|| format!("open guard file {:?}", path))?;

        if try_lock_exclusive_nonblocking(&file)? {
            Ok(Some(Self { file }))
        } else {
            Ok(None)
        }
    }

    fn touch(&self) {
        let _ = self.file.metadata();
    }
}

pub struct SessionLock {
    path: PathBuf,
    expected_contents: String,
    guard: Option<GuardFile>,
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

        let guard = match GuardFile::acquire(chat_dir)? {
            Some(guard) => guard,
            None => return Err(live_owner_error(chat_dir)?),
        };

        let expected_contents = format_lock_contents(std::process::id(), kind);
        match read_holder_at(&path)? {
            Some(holder) if holder.alive => {
                return Err(anyhow!(
                    "session lock held by live handler pid={} kind={} started={}",
                    holder.pid,
                    holder.kind.map(|k| k.label()).unwrap_or("unknown"),
                    holder.started_at
                ));
            }
            Some(holder) => {
                eprintln!(
                    "[session-lock] recovering stale lock (dead pid={}, kind={}) at {:?}",
                    holder.pid,
                    holder.kind.map(|k| k.label()).unwrap_or("unknown"),
                    path
                );
            }
            None if path.exists() => {
                eprintln!("[session-lock] recovering unparseable lock at {:?}", path);
            }
            None => {}
        }

        write_lock_contents(&path, &expected_contents)?;

        Ok(Self {
            path,
            expected_contents,
            guard: Some(guard),
            released: false,
        })
    }

    pub fn release(&mut self) {
        if self.released {
            return;
        }

        if let Some(guard) = self.guard.as_ref() {
            guard.touch();
            match std::fs::read_to_string(&self.path) {
                Ok(current) if current == self.expected_contents => {
                    if let Err(err) = std::fs::remove_file(&self.path) {
                        eprintln!(
                            "[session-lock] warning: failed to remove lock {:?}: {}",
                            self.path, err
                        );
                    }
                }
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    eprintln!(
                        "[session-lock] warning: failed to read lock {:?} during release: {}",
                        self.path, err
                    );
                }
            }
        }

        self.guard = None;
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

fn live_owner_error(chat_dir: &Path) -> Result<anyhow::Error> {
    let start = std::time::Instant::now();
    loop {
        match read_holder(chat_dir)? {
            Some(holder) if holder.alive => {
                return Ok(anyhow!(
                    "session lock held by live handler pid={} kind={} started={}",
                    holder.pid,
                    holder.kind.map(|k| k.label()).unwrap_or("unknown"),
                    holder.started_at
                ));
            }
            Some(_) => {}
            None => {}
        }

        if start.elapsed() >= OWNER_METADATA_WAIT {
            return Ok(anyhow!(
                "session lock is currently being acquired by another handler"
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn guard_path(chat_dir: &Path) -> PathBuf {
    chat_dir.join(GUARD_FILENAME)
}

fn format_lock_contents(pid: u32, kind: HandlerKind) -> String {
    format!(
        "{}\n{}\n{}\n",
        pid,
        chrono::Utc::now().to_rfc3339(),
        kind.label(),
    )
}

fn write_lock_contents(path: &Path, contents: &str) -> Result<()> {
    let tmp_path = path.with_extension("tmp");
    let mut tmp = open_write_truncate(&tmp_path)
        .with_context(|| format!("open temp lock file {:?}", tmp_path))?;
    tmp.write_all(contents.as_bytes())
        .with_context(|| format!("write temp lock file {:?}", tmp_path))?;
    tmp.sync_all()
        .with_context(|| format!("fsync temp lock file {:?}", tmp_path))?;
    drop(tmp);

    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("rename temp lock file {:?}", path))?;
    Ok(())
}

fn open_write_truncate(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    options.open(path)
}

#[cfg(unix)]
fn try_lock_exclusive_nonblocking(file: &File) -> Result<bool> {
    use std::os::fd::AsRawFd;

    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(true)
    } else {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(anyhow!("acquire guard flock: {}", err)),
        }
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive_nonblocking(_file: &File) -> Result<bool> {
    Err(anyhow!(
        "session lock is unsupported on this platform until a real cross-platform guard is implemented"
    ))
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
            err.contains("pid=") || err.contains("being acquired"),
            "error must identify the live owner or in-progress owner: {}",
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
    fn release_does_not_remove_lock_when_contents_change() {
        let dir = tempdir().unwrap();
        let mut lock = SessionLock::acquire(dir.path(), HandlerKind::ChatNex).unwrap();
        let path = SessionLock::lock_path(dir.path());
        let replacement = "424242\n2026-01-01T00:00:00Z\nadapter\n";
        std::fs::write(&path, replacement).unwrap();

        lock.release();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), replacement);
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
