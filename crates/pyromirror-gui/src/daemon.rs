//! Long-lived PyroMirror processes that outlive the launcher window: the sharing server and the
//! tray agent. Each is tracked through a pid file and (for the server) a log file in the config
//! directory, so the window and the tray agent see and control the same instance.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pyromirror_proto::auth;

use crate::process::{parse_line, LogLine};

const MAX_LOG_LINES: usize = 200;

pub struct Daemon {
    /// File name of the program, without extension.
    exe: &'static str,
    pid_file: PathBuf,
    log_file: PathBuf,
    /// Set if this process started it, so the child can be reaped when it exits.
    child: Mutex<Option<Child>>,
}

fn state_dir() -> PathBuf {
    auth::config_dir().unwrap_or_else(std::env::temp_dir)
}

pub fn sibling(name: &str) -> PathBuf {
    let file = format!("{}{}", name, std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&file)))
        .filter(|path| path.exists())
        // Fall back to PATH.
        .unwrap_or_else(|| PathBuf::from(file))
}

/// Starts `program` so that it keeps running after this process exits.
fn spawn_detached(program: PathBuf, args: &[String], stderr: Stdio) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so closing our terminal or session leader does not take it down.
        command.process_group(0);
    }
    command.spawn()
}

impl Daemon {
    fn new(exe: &'static str, name: &str) -> Self {
        let dir = state_dir();
        Self {
            exe,
            pid_file: dir.join(format!("{name}.pid")),
            log_file: dir.join(format!("{name}.log")),
            child: Mutex::new(None),
        }
    }

    /// The sharing server (`pyromirror-server`).
    pub fn server() -> Self {
        Self::new("pyromirror-server", "server")
    }

    /// The remote desktop window (`pyromirror-client`).
    pub fn viewer() -> Self {
        Self::new("pyromirror-client", "viewer")
    }

    /// The tray agent (`pyromirror --background`).
    pub fn agent() -> Self {
        Self::new("pyromirror", "agent")
    }

    /// The launcher window.
    pub fn window() -> Self {
        Self::new("pyromirror", "window")
    }

    /// The process id recorded in the pid file, if that process is still alive.
    pub fn pid(&self) -> Option<u32> {
        // Reap our own child first: a zombie would otherwise look alive.
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.try_wait();
        }
        let pid: u32 = fs::read_to_string(&self.pid_file).ok()?.trim().parse().ok()?;
        sys::is_alive(pid, self.exe).then_some(pid)
    }

    pub fn is_running(&self) -> bool {
        self.pid().is_some()
    }

    /// Starts the program unless it is already running. Its log starts afresh.
    pub fn start(&self, args: &[String]) -> std::io::Result<()> {
        if self.is_running() {
            return Ok(());
        }
        if let Some(dir) = self.pid_file.parent() {
            fs::create_dir_all(dir)?;
        }
        let log = fs::File::create(&self.log_file)?;
        let child = spawn_detached(sibling(self.exe), args, Stdio::from(log))?;
        fs::write(&self.pid_file, child.id().to_string())?;
        *self.child.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Records the current process as the running instance. Returns false if another one is.
    pub fn claim(&self) -> bool {
        match self.pid() {
            Some(pid) if pid != std::process::id() => false,
            _ => {
                if let Some(dir) = self.pid_file.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                fs::write(&self.pid_file, std::process::id().to_string()).is_ok()
            }
        }
    }

    /// Removes the record made by [`claim`](Self::claim).
    pub fn release(&self) {
        if self.pid() == Some(std::process::id()) {
            let _ = fs::remove_file(&self.pid_file);
        }
    }

    /// Stops the program and waits briefly for it to go.
    pub fn stop(&self) {
        if let Some(pid) = self.pid() {
            sys::terminate(pid);
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.is_running() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = fs::remove_file(&self.pid_file);
        *self.child.lock().unwrap() = None;
    }

    /// The most recent lines the program wrote to its log.
    pub fn log(&self) -> Vec<LogLine> {
        let text = fs::read(&self.log_file).unwrap_or_default();
        let text = String::from_utf8_lossy(&text);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        lines[lines.len().saturating_sub(MAX_LOG_LINES)..].iter().map(|l| parse_line(l)).collect()
    }
}

#[cfg(target_os = "linux")]
mod sys {
    pub fn is_alive(pid: u32, exe: &str) -> bool {
        // The kernel truncates the name to 15 characters. Checking it guards against the pid
        // having been reused by something else.
        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else { return false };
        if !exe.starts_with(comm.trim()) {
            return false;
        }
        // State is the first field after the parenthesised name; Z is a zombie.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        !matches!(stat.rsplit_once(") ").and_then(|(_, rest)| rest.chars().next()), Some('Z') | None)
    }

    pub fn terminate(pid: u32) {
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
    }
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    pub fn is_alive(pid: u32, _exe: &str) -> bool {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }

    pub fn terminate(pid: u32) {
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn tracks_a_real_process_through_its_pid_file() {
        let dir = std::env::temp_dir().join(format!("pyromirror-daemon-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // `sleep` stands in for the server.
        let daemon = Daemon {
            exe: "sleep",
            pid_file: dir.join("t.pid"),
            log_file: dir.join("t.log"),
            child: Mutex::new(None),
        };
        assert!(!daemon.is_running());

        let child = spawn_detached(PathBuf::from("sleep"), &["30".into()], Stdio::null()).unwrap();
        fs::write(&daemon.pid_file, child.id().to_string()).unwrap();
        *daemon.child.lock().unwrap() = Some(child);
        assert!(daemon.is_running());

        // A second handle (another program) sees and can stop the same instance.
        let other = Daemon { exe: "sleep", pid_file: dir.join("t.pid"), log_file: dir.join("t.log"), child: Mutex::new(None) };
        assert!(other.is_running());
        // A pid that belongs to a different program does not count.
        let wrong = Daemon { exe: "pyromirror-server", pid_file: dir.join("t.pid"), log_file: dir.join("t.log"), child: Mutex::new(None) };
        assert!(!wrong.is_running());

        other.stop();
        assert!(!daemon.is_running(), "the owner must notice its child is gone, not see a zombie");

        fs::write(&daemon.log_file, "[2026-01-01T00:00:00Z INFO  x] Listening on 0.0.0.0:9000\n\nError: boom\n").unwrap();
        let log = daemon.log();
        assert_eq!(log.len(), 2);
        assert!(log[0].text.starts_with("Listening") && log[1].text == "Error: boom");
        let _ = fs::remove_dir_all(dir);
    }
}
