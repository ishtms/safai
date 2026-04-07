//! cross-platform kill_pid via sysinfo.
//!
//! routes through [`sysinfo::Process::kill`] / [`sysinfo::Process::kill_with`]
//! so we don't need direct libc / win32 deps.
//!
//! * unix: force=false sends SIGTERM (polite), force=true SIGKILL
//! * windows: force ignored, TerminateProcess is the only primitive. arg still
//!   accepted so the Tauri command signature stays cross-platform
//!
//! guardrails above sysinfo refuse pid 0 (swapper / System Idle),
//! pid 1 (init / launchd / wininit), and our own pid.

use std::fmt;

use sysinfo::{Pid, ProcessesToUpdate, Signal, System};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillError {
    /// init, swapper, or our own pid
    Protected(u32),
    /// already gone or never alive
    NotFound(u32),
    /// OS refused, usually means we don't own the target
    Refused(u32),
}

impl fmt::Display for KillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KillError::Protected(pid) => write!(f, "refused to kill protected pid {pid}"),
            KillError::NotFound(pid) => write!(f, "process {pid} not found"),
            KillError::Refused(pid) => write!(f, "OS refused to kill pid {pid}"),
        }
    }
}

impl std::error::Error for KillError {}

impl From<KillError> for String {
    fn from(e: KillError) -> Self {
        e.to_string()
    }
}

/// pure so tests can cover without fork(2)
pub fn is_protected_pid(pid: u32, current_pid: u32) -> bool {
    pid == 0 || pid == 1 || pid == current_pid
}

/// force=true is SIGKILL on unix, ignored on windows (only TerminateProcess exists)
pub fn kill_pid(pid: u32, force: bool) -> Result<(), KillError> {
    let current = std::process::id();
    if is_protected_pid(pid, current) {
        return Err(KillError::Protected(pid));
    }
    let mut sys = System::new();
    let target = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    let Some(proc_) = sys.process(target) else {
        return Err(KillError::NotFound(pid));
    };
    let ok = if force {
        proc_.kill()
    } else {
        // SIGTERM on unix. windows returns None (no signal api), fall back to
        // kill() which is TerminateProcess
        proc_.kill_with(Signal::Term).unwrap_or_else(|| proc_.kill())
    };
    if ok {
        Ok(())
    } else {
        Err(KillError::Refused(pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_zero_protected() {
        assert!(is_protected_pid(0, 12345));
    }

    #[test]
    fn pid_one_protected() {
        assert!(is_protected_pid(1, 12345));
    }

    #[test]
    fn self_pid_protected() {
        assert!(is_protected_pid(42, 42));
    }

    #[test]
    fn other_pid_not_protected() {
        assert!(!is_protected_pid(9999, 1));
    }

    #[test]
    fn kill_zero_pid_is_protected() {
        let err = kill_pid(0, false).unwrap_err();
        assert!(matches!(err, KillError::Protected(0)));
    }

    #[test]
    fn kill_init_pid_is_protected() {
        let err = kill_pid(1, true).unwrap_err();
        assert!(matches!(err, KillError::Protected(1)));
    }

    #[test]
    fn kill_self_pid_is_protected() {
        let me = std::process::id();
        let err = kill_pid(me, false).unwrap_err();
        assert!(matches!(err, KillError::Protected(p) if p == me));
    }

    #[test]
    fn kill_unknown_pid_returns_not_found() {
        // u32::MAX is rejected by most OSes as out-of-range. sysinfo refresh
        // returns empty so we surface NotFound, not Refused
        let err = kill_pid(u32::MAX, false).unwrap_err();
        assert!(
            matches!(err, KillError::NotFound(_) | KillError::Refused(_)),
            "got {err:?}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_real_child_succeeds() {
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::{Duration, Instant};
        // don't rely on `sleep` being on PATH, use sh with a pause
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        // let child register in the process table
        thread::sleep(Duration::from_millis(50));
        kill_pid(pid, true).expect("kill_pid");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_status) => break,
                None => {
                    if Instant::now() > deadline {
                        child.kill().ok();
                        panic!("child never exited after kill_pid");
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn kill_graceful_then_force_sequence() {
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::{Duration, Instant};
        // trap swallows SIGTERM so graceful "succeeds" from sysinfo's view but
        // the process keeps running. force=true SIGKILL must actually reap it
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        thread::sleep(Duration::from_millis(50));
        // may or may not terminate
        let _ = kill_pid(pid, false);
        thread::sleep(Duration::from_millis(100));
        // SIGKILL is uncatchable
        kill_pid(pid, true).expect("force kill");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                break;
            }
            if Instant::now() > deadline {
                child.kill().ok();
                panic!("child never exited after force kill");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
