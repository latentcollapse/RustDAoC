//! Portable exclusive init lock (LANE PLT).
//!
//! Serializes wgpu adapter/device init across threads **and** processes. Unix uses `flock`;
//! Windows uses `LockFileEx`. Both are kernel-owned: the OS releases the lock if the holder
//! process dies. There is no mkdir/steal path.
//!
//! Fail closed: timeout, permission, and acquire failures are [`InitLockError`]. Callers must
//! never proceed with unlocked init.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(not(any(unix, windows)))]
compile_error!("caer-render init_lock requires cfg(unix) or cfg(windows)");

/// Typed fail-closed lock errors. Never mapped to a successful unlocked guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitLockError {
    Timeout { path: PathBuf, waited_secs: u64 },
    Permission { path: PathBuf, detail: String },
    Open { path: PathBuf, detail: String },
    Acquire { path: PathBuf, detail: String },
}

impl std::fmt::Display for InitLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { path, waited_secs } => write!(
                f,
                "timed out after {waited_secs}s waiting for exclusive init lock on {} \
                 (holder process death is kernel-released; this is not a steal path)",
                path.display()
            ),
            Self::Permission { path, detail } => {
                write!(
                    f,
                    "permission denied for init lock {}: {detail}",
                    path.display()
                )
            }
            Self::Open { path, detail } => {
                write!(f, "open init lock {}: {detail}", path.display())
            }
            Self::Acquire { path, detail } => {
                write!(f, "acquire init lock {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for InitLockError {}

/// Exclusive lock guard. Drop unlocks (and process death unlocks via the kernel).
#[derive(Debug)]
pub struct InitLockGuard {
    file: Option<File>,
}

impl Drop for InitLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            platform_unlock(&file);
        }
    }
}

/// Acquire an exclusive lock on `path` or return a typed error. Never returns a guard without
/// holding the OS lock.
pub fn lock_exclusive(path: &Path, timeout: Duration) -> Result<InitLockGuard, InitLockError> {
    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return Err(InitLockError::Permission {
                path: path.to_path_buf(),
                detail: e.to_string(),
            });
        }
        Err(e) => {
            return Err(InitLockError::Open {
                path: path.to_path_buf(),
                detail: e.to_string(),
            });
        }
    };

    let deadline = Instant::now() + timeout;
    loop {
        match platform_try_lock(&file) {
            LockAttempt::Acquired => {
                return Ok(InitLockGuard { file: Some(file) });
            }
            LockAttempt::Contended => {
                if Instant::now() >= deadline {
                    return Err(InitLockError::Timeout {
                        path: path.to_path_buf(),
                        waited_secs: timeout.as_secs().max(1),
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            LockAttempt::Permission(detail) => {
                return Err(InitLockError::Permission {
                    path: path.to_path_buf(),
                    detail,
                });
            }
            LockAttempt::Failed(detail) => {
                return Err(InitLockError::Acquire {
                    path: path.to_path_buf(),
                    detail,
                });
            }
        }
    }
}

/// One non-blocking exclusive attempt. `Ok(None)` is contention, never an unlocked guard.
pub fn try_exclusive(path: &Path) -> Result<Option<InitLockGuard>, InitLockError> {
    match lock_exclusive(path, Duration::ZERO) {
        Ok(g) => Ok(Some(g)),
        Err(InitLockError::Timeout { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

enum LockAttempt {
    Acquired,
    Contended,
    Permission(String),
    Failed(String),
}

#[cfg(unix)]
fn platform_try_lock(file: &File) -> LockAttempt {
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return LockAttempt::Acquired;
    }
    let err = std::io::Error::last_os_error();
    match err.kind() {
        ErrorKind::WouldBlock => LockAttempt::Contended,
        ErrorKind::PermissionDenied => LockAttempt::Permission(err.to_string()),
        _ => {
            // EINTR is retryable contention-adjacent; treat as contended until timeout.
            if err.raw_os_error() == Some(libc::EINTR) {
                LockAttempt::Contended
            } else {
                LockAttempt::Failed(err.to_string())
            }
        }
    }
}

#[cfg(unix)]
fn platform_unlock(file: &File) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

#[cfg(windows)]
fn platform_try_lock(file: &File) -> LockAttempt {
    windows_try_lock(file)
}

#[cfg(windows)]
fn platform_unlock(file: &File) {
    windows_unlock(file);
}

#[cfg(windows)]
mod win {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    pub const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    pub const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    pub const ERROR_LOCK_VIOLATION: u32 = 33;
    pub const ERROR_IO_PENDING: u32 = 997;
    pub const ERROR_ACCESS_DENIED: u32 = 5;
    pub const ERROR_SHARING_VIOLATION: u32 = 32;

    #[repr(C)]
    pub struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut core::ffi::c_void,
    }

    impl Overlapped {
        pub fn zero() -> Self {
            Self {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: std::ptr::null_mut(),
            }
        }
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            h_file: *mut core::ffi::c_void,
            dw_flags: u32,
            dw_reserved: u32,
            n_number_of_bytes_to_lock_low: u32,
            n_number_of_bytes_to_lock_high: u32,
            lp_overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            h_file: *mut core::ffi::c_void,
            dw_reserved: u32,
            n_number_of_bytes_to_unlock_low: u32,
            n_number_of_bytes_to_unlock_high: u32,
            lp_overlapped: *mut Overlapped,
        ) -> i32;
        fn GetLastError() -> u32;
    }

    pub fn try_lock(file: &File) -> super::LockAttempt {
        let mut ov = Overlapped::zero();
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
                0,
                1,
                0,
                &mut ov,
            )
        };
        if ok != 0 {
            return super::LockAttempt::Acquired;
        }
        let code = unsafe { GetLastError() };
        match code {
            ERROR_LOCK_VIOLATION | ERROR_IO_PENDING | ERROR_SHARING_VIOLATION => {
                super::LockAttempt::Contended
            }
            ERROR_ACCESS_DENIED => super::LockAttempt::Permission(format!("GetLastError={code}")),
            other => super::LockAttempt::Failed(format!("LockFileEx GetLastError={other}")),
        }
    }

    pub fn unlock(file: &File) {
        let mut ov = Overlapped::zero();
        unsafe {
            UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut ov);
        }
    }
}

#[cfg(windows)]
fn windows_try_lock(file: &File) -> LockAttempt {
    win::try_lock(file)
}

#[cfg(windows)]
fn windows_unlock(file: &File) {
    win::unlock(file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc, Barrier};
    use std::thread;

    fn unique_lock_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("caer-init-lock-tests");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!(
            "{tag}-{}-{}.lock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// Falsifier: uncontended lock succeeds; Drop unlocks so a second acquire works.
    #[test]
    fn uncontended_lock_is_reusable_after_drop() {
        let path = unique_lock_path("reuse");
        let g = lock_exclusive(&path, Duration::from_secs(5)).expect("uncontended must succeed");
        drop(g);
        let g2 = lock_exclusive(&path, Duration::from_secs(5)).expect("reusable after Drop");
        drop(g2);
        let _ = std::fs::remove_file(&path);
    }

    /// Falsifier: contended lock times out as Timeout — never Ok without holding the lock.
    #[test]
    fn timeout_is_typed_never_unlocked_ok() {
        let path = unique_lock_path("timeout");
        let held = lock_exclusive(&path, Duration::from_secs(5)).expect("holder");
        let (tx, rx) = mpsc::channel();
        let path_t = path.clone();
        let h = thread::spawn(move || {
            let r = lock_exclusive(&path_t, Duration::from_millis(250));
            let _ = tx.send(r.map(|_| ()));
        });
        let second = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waiter must return");
        match second {
            Err(InitLockError::Timeout { .. }) => {}
            other => panic!("expected Timeout, got {other:?} — unlocked init is forbidden"),
        }
        drop(held);
        h.join().expect("waiter thread");
        let _ = std::fs::remove_file(&path);
    }

    /// Falsifier: two waiters cannot both succeed while one holder lives.
    #[test]
    fn contended_second_acquire_does_not_succeed() {
        let path = unique_lock_path("contend");
        let barrier = Arc::new(Barrier::new(2));
        let held = lock_exclusive(&path, Duration::from_secs(5)).unwrap();
        let path_t = path.clone();
        let b = Arc::clone(&barrier);
        let h = thread::spawn(move || {
            b.wait();
            lock_exclusive(&path_t, Duration::from_millis(150))
        });
        barrier.wait();
        let err = h.join().unwrap().expect_err("must not acquire while held");
        assert!(matches!(err, InitLockError::Timeout { .. }), "got {err:?}");
        drop(held);
        let _ = std::fs::remove_file(&path);
    }

    /// Falsifier: chmod 000 → Permission (or Open mapped from EACCES), never a guard.
    #[cfg(unix)]
    #[test]
    fn permission_denied_is_typed() {
        use std::os::unix::fs::PermissionsExt;
        let path = unique_lock_path("perm");
        std::fs::write(&path, b"").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();
        let err = lock_exclusive(&path, Duration::from_secs(1)).expect_err("chmod 000 must fail");
        // Restore so cleanup can unlink.
        let mut perms = std::fs::metadata(&path)
            .map(|m| m.permissions())
            .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o644));
        perms.set_mode(0o644);
        let _ = std::fs::set_permissions(&path, perms);
        assert!(
            matches!(
                err,
                InitLockError::Permission { .. } | InitLockError::Open { .. }
            ),
            "got {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Owner-death: child holds the lock, `_exit`s without Drop; kernel must release.
    /// Discriminating vs mkdir-steal: we never unlink the lock file to recover.
    #[cfg(unix)]
    #[test]
    fn owner_death_kernel_releases_lock() {
        let path = if let Ok(p) = std::env::var("CAER_INIT_LOCK_PATH") {
            PathBuf::from(p)
        } else {
            unique_lock_path("owner-death")
        };
        if std::env::var("CAER_INIT_LOCK_CHILD").ok().as_deref() == Some("1") {
            let g = lock_exclusive(&path, Duration::from_secs(5)).expect("child acquire");
            std::mem::forget(g);
            unsafe { libc::_exit(0) };
        }
        std::fs::write(&path, b"").unwrap();
        let exe = std::env::current_exe().expect("test exe");
        let status = std::process::Command::new(exe)
            .arg("--exact")
            .arg("init_lock::tests::owner_death_kernel_releases_lock")
            .env("CAER_INIT_LOCK_CHILD", "1")
            .env("CAER_INIT_LOCK_PATH", &path)
            .env("RUST_TEST_THREADS", "1")
            .status()
            .expect("spawn child");
        assert!(status.success(), "child _exit must succeed: {status}");
        let g = lock_exclusive(&path, Duration::from_secs(5))
            .expect("kernel must release flock after holder process death");
        drop(g);
        let _ = std::fs::remove_file(&path);
    }

    /// Display names timeout/permission so GpuInitError::InitLock stays actionable.
    #[test]
    fn error_display_is_actionable() {
        let t = InitLockError::Timeout {
            path: PathBuf::from("/tmp/x.lock"),
            waited_secs: 60,
        };
        let s = t.to_string();
        assert!(s.contains("timed out"), "{s}");
        assert!(s.contains("kernel-released"), "{s}");
        let p = InitLockError::Permission {
            path: PathBuf::from("/tmp/x.lock"),
            detail: "EACCES".into(),
        };
        assert!(p.to_string().contains("permission denied"));
    }
}
