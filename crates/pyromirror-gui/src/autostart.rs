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
/// program was moved or updated.
#[cfg(target_os = "linux")]
pub fn set(enabled: bool) -> std::io::Result<()> {
    let path = entry_path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let (exe, arg) = command_line().ok_or_else(|| std::io::Error::other("cannot find the PyroMirror program"))?;
    write_entry(&path, enabled.then_some((exe.as_path(), arg)))
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
}
