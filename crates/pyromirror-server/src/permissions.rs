//! `--check-permissions`: finds out, with a person at the desk, whether sharing can later start
//! without one. Anything the system needs to ask is asked now.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use anyhow::{bail, Context};
use log::{info, warn};

use pyromirror_capture::{CaptureOptions, Capturer};
use pyromirror_codec::Device;
use pyromirror_net::create_streaming_socket;

/// Runs the checks. The launcher looks for the "Permission check" lines.
pub fn check(bind: SocketAddr, monitor: Option<u32>, want_input: bool) -> anyhow::Result<()> {
    info!("Permission check: asking the desktop for access (look for a dialog)");
    let mut capturer = Capturer::new(&CaptureOptions { output: monitor }).context("screen capture was not allowed")?;

    // Seeing a frame proves the grant actually works, not just that a dialog was answered.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if capturer.next_frame(Duration::from_millis(200)).context("screen capture stopped")?.is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            bail!("screen capture was allowed but delivered no picture");
        }
    }

    if !capturer.permission_remembered() {
        bail!(
            "your desktop did not remember the permission, so it would ask again at every start; \
             tick the option to remember or restore the session in its dialog and try again"
        );
    }
    if want_input && capturer.input_injector().is_none() {
        warn!("Permission check: this desktop does not allow remote input; sharing will be view-only");
    }

    Device::new().context("the graphics driver cannot run the PyroWave codec")?;

    // Listening is what makes Windows show its firewall prompt.
    let _listener = TcpListener::bind(bind).with_context(|| format!("could not listen on {}", bind))?;
    let _udp = create_streaming_socket(bind, 1 << 20).with_context(|| format!("could not bind UDP {}", bind))?;
    firewall::wait_until_allowed()?;

    info!("Permission check passed");
    Ok(())
}

#[cfg(not(windows))]
mod firewall {
    pub fn wait_until_allowed() -> anyhow::Result<()> {
        // Linux desktops do not gate listening behind a per-application prompt. A firewall the
        // administrator configured (firewalld, ufw) is outside what we can ask for.
        Ok(())
    }
}

#[cfg(windows)]
mod firewall {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use anyhow::bail;
    use log::{info, warn};

    #[derive(PartialEq)]
    enum Rule {
        Allowed,
        Blocked,
        Missing,
        Unknown,
    }

    /// Asks Windows Defender Firewall about inbound rules for this executable.
    fn query() -> Rule {
        let Ok(exe) = std::env::current_exe() else { return Rule::Unknown };
        let script = format!(
            "$on = @(Get-NetFirewallProfile | Where-Object {{ $_.Enabled -eq 'True' }}).Count; \
             if ($on -eq 0) {{ 'allowed'; exit }}; \
             $r = @(Get-NetFirewallApplicationFilter -Program '{}' -ErrorAction SilentlyContinue | Get-NetFirewallRule | \
                    Where-Object {{ $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound' }}); \
             if (@($r | Where-Object {{ $_.Action -eq 'Block' }}).Count -gt 0) {{ 'blocked' }} \
             elseif (@($r | Where-Object {{ $_.Action -eq 'Allow' }}).Count -gt 0) {{ 'allowed' }} else {{ 'missing' }}",
            exe.display().to_string().replace('\'', "''")
        );
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match output {
            Ok(out) => match String::from_utf8_lossy(&out.stdout).trim() {
                "allowed" => Rule::Allowed,
                "blocked" => Rule::Blocked,
                "missing" => Rule::Missing,
                _ => Rule::Unknown,
            },
            Err(_) => Rule::Unknown,
        }
    }

    pub fn wait_until_allowed() -> anyhow::Result<()> {
        info!("Permission check: waiting for the Windows firewall to allow connections (look for its prompt)");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            match query() {
                Rule::Allowed => return Ok(()),
                Rule::Blocked => bail!(
                    "the Windows firewall blocks PyroMirror; allow \"pyromirror-server\" under \
                     \"Allow an app through Windows Firewall\" and try again"
                ),
                Rule::Unknown => {
                    warn!("Permission check: could not read the firewall rules; assuming they allow connections");
                    return Ok(());
                }
                Rule::Missing if Instant::now() >= deadline => {
                    bail!("the Windows firewall prompt was not answered; try again and choose \"Allow\"")
                }
                Rule::Missing => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    }
}
