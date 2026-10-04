//! Starting PyroMirror (in the tray) when the user logs in.
//!
//! - Windows: a value under the user's `Run` registry key.
//! - Linux: an XDG autostart entry, which GNOME, KDE and most other desktops honour.

use std::path::PathBuf;

fn command_line() -> Option<(PathBuf, &'static str)> {
    Some((std::env::current_exe().ok()?, "--background"))
}

#[cfg(target_os = "linux")]
fn entry_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("autostart").join("pyromirror.desktop"))
}

/// Creates or removes the login entry. Enabling also refreshes the recorded path, in case the
/// program was moved or updated. An entry that starts a copy installed from a package is left
/// as it is when this is some other copy, such as one built from source and run for a moment.
#[cfg(target_os = "linux")]
pub fn set(enabled: bool) -> std::io::Result<()> {
    let path = entry_path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let (exe, arg) = command_line().ok_or_else(|| std::io::Error::other("cannot find the PyroMirror program"))?;
    if enabled {
        let existing = std::fs::read_to_string(&path).ok().and_then(|entry| entry_program(&entry));
        if let Some(installed) = existing.filter(|p| p.exists()) {
            if belongs_to_installed(&installed, &exe) {
                log::info!("Login starts the installed PyroMirror ({}); leaving that as it is", installed.display());
                return Ok(());
            }
        }
    }
    write_entry(&path, enabled.then_some((exe.as_path(), arg)))
}

/// The program a login entry starts.
#[cfg(target_os = "linux")]
fn entry_program(entry: &str) -> Option<PathBuf> {
    let exec = entry.lines().find_map(|line| line.strip_prefix("Exec="))?;
    let Some(quoted) = exec.strip_prefix('"') else {
        return exec.split_whitespace().next().map(PathBuf::from);
    };
    // The reverse of the quoting in `write_entry`.
    let mut program = String::new();
    let mut chars = quoted.chars();
    loop {
        match chars.next()? {
            '"' => return Some(PathBuf::from(program)),
            '\\' => program.push(chars.next()?),
            c => program.push(c),
        }
    }
}

/// Where packages put programs, as opposed to a home or build directory.
#[cfg(target_os = "linux")]
fn is_installed(program: &std::path::Path) -> bool {
    program.starts_with("/usr") || program.starts_with("/opt")
}

/// Whether the login entry, which starts `existing`, should stay with it rather than be pointed
/// at `exe`, the copy running now.
#[cfg(target_os = "linux")]
fn belongs_to_installed(existing: &std::path::Path, exe: &std::path::Path) -> bool {
    existing != exe && is_installed(existing) && !is_installed(exe)
}

#[cfg(target_os = "linux")]
fn write_entry(path: &std::path::Path, command: Option<(&std::path::Path, &str)>) -> std::io::Result<()> {
    let Some((exe, arg)) = command else {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    };
    // Desktop entries quote arguments with double quotes; escape what the spec reserves.
    let quoted: String = exe
        .display()
        .to_string()
        .chars()
        .flat_map(|c| if matches!(c, '"' | '`' | '$' | '\\') { vec!['\\', c] } else { vec![c] })
        .collect();
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=PyroMirror\nComment=Remote desktop, running in the tray\n\
         Exec=\"{quoted}\" {arg}\nIcon=pyromirror\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, entry)
}

#[cfg(target_os = "linux")]
pub fn is_set() -> bool {
    entry_path().is_some_and(|p| p.exists())
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
fn reg(args: &[&str]) -> std::io::Result<bool> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Ok(std::process::Command::new("reg.exe").args(args).creation_flags(CREATE_NO_WINDOW).output()?.status.success())
}

#[cfg(windows)]
pub fn set(enabled: bool) -> std::io::Result<()> {
    if !enabled {
        // Fails if the value is already gone, which is fine.
        reg(&["delete", RUN_KEY, "/v", "PyroMirror", "/f"])?;
        return Ok(());
    }
    let (exe, arg) = command_line().ok_or_else(|| std::io::Error::other("cannot find the PyroMirror program"))?;
    let value = format!("\"{}\" {}", exe.display(), arg);
    if reg(&["add", RUN_KEY, "/v", "PyroMirror", "/t", "REG_SZ", "/d", &value, "/f"])? {
        Ok(())
    } else {
        Err(std::io::Error::other("could not write the startup entry to the registry"))
    }
}

#[cfg(windows)]
pub fn is_set() -> bool {
    reg(&["query", RUN_KEY, "/v", "PyroMirror"]).unwrap_or(false)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn entry_is_written_quoted_and_removed() {
        let dir = std::env::temp_dir().join(format!("pyromirror-autostart-test-{}", std::process::id()));
        let path = dir.join("autostart/pyromirror.desktop");
        let exe = std::path::Path::new("/opt/My Apps/pyro$mirror");
        write_entry(&path, Some((exe, "--background"))).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=\"/opt/My Apps/pyro\\$mirror\" --background\n"), "{text}");
        assert!(text.starts_with("[Desktop Entry]\nType=Application\n"));

        write_entry(&path, None).unwrap();
        assert!(!path.exists());
        // Removing what is not there is fine.
        write_entry(&path, None).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_program_of_an_entry_is_read_back() {
        let dir = std::env::temp_dir().join(format!("pyromirror-autostart-read-{}", std::process::id()));
        let path = dir.join("pyromirror.desktop");
        let exe = std::path::Path::new("/opt/My Apps/pyro$mirror");
        write_entry(&path, Some((exe, "--background"))).unwrap();
        assert_eq!(entry_program(&std::fs::read_to_string(&path).unwrap()).as_deref(), Some(exe));
        let _ = std::fs::remove_dir_all(dir);

        assert_eq!(entry_program("[Desktop Entry]\nExec=/usr/bin/pyromirror --background\n"), Some("/usr/bin/pyromirror".into()));
        assert_eq!(entry_program("[Desktop Entry]\nName=PyroMirror\n"), None);
        assert_eq!(entry_program("Exec=\"/usr/bin/unterminated"), None);
    }

    #[test]
    fn an_installed_copy_keeps_the_login_entry() {
        use std::path::Path;
        let (installed, built) = (Path::new("/usr/bin/pyromirror"), Path::new("/home/me/src/target/release/pyromirror"));
        // A copy built from source does not take over from the package.
        assert!(belongs_to_installed(installed, built));
        // The package itself refreshes its entry, and takes over from anything else.
        assert!(!belongs_to_installed(installed, installed));
        assert!(!belongs_to_installed(built, installed));
        assert!(!belongs_to_installed(Path::new("/usr/bin/pyromirror"), Path::new("/opt/pyromirror/pyromirror")));
        // Two copies outside any package: the one in use wins, as before.
        assert!(!belongs_to_installed(built, Path::new("/home/me/bin/pyromirror")));
    }
}
