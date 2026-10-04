//! Keeps the stream going while a game has the computer busy.

/// Puts this process ahead of ordinary ones for processor and graphics time.
///
/// A game uses all of the graphics adapter it can get, and the encoder's work would otherwise
/// queue up behind the game's own frames. Does nothing where that cannot be asked for.
pub fn raise() {
    #[cfg(windows)]
    windows::raise();
}

#[cfg(windows)]
mod windows {
    use log::{debug, info};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, HIGH_PRIORITY_CLASS};

    // D3DKMT_SCHEDULINGPRIORITYCLASS. "Realtime" is left alone: it can starve the game and the
    // desktop itself.
    const GPU_PRIORITIES: [(i32, &str); 2] = [(4, "high"), (3, "above normal")];

    /// `D3DKMTSetProcessSchedulingPriorityClass`; returns an NTSTATUS.
    type SetGpuPriority = unsafe extern "system" fn(HANDLE, i32) -> i32;

    pub fn raise() {
        let process = unsafe { GetCurrentProcess() };
        if unsafe { SetPriorityClass(process, HIGH_PRIORITY_CLASS) } == 0 {
            debug!("Could not raise the process priority");
        }

        // Looked up by name: not every toolchain's import library lists it.
        let set_gpu_priority: Option<SetGpuPriority> = unsafe {
            let gdi32 = LoadLibraryA(b"gdi32.dll\0".as_ptr());
            if gdi32.is_null() {
                None
            } else {
                GetProcAddress(gdi32, b"D3DKMTSetProcessSchedulingPriorityClass\0".as_ptr())
                    .map(|function| std::mem::transmute::<_, SetGpuPriority>(function))
            }
        };
        let Some(set_gpu_priority) = set_gpu_priority else {
            debug!("This Windows cannot set a graphics scheduling priority");
            return;
        };
        // The higher one is only granted to a process that runs as administrator.
        match GPU_PRIORITIES.iter().find(|(class, _)| unsafe { set_gpu_priority(process, *class) } >= 0) {
            Some((_, name)) => info!("Graphics scheduling priority: {}", name),
            None => info!("Could not raise the graphics scheduling priority; a game may make the stream stutter"),
        }
    }
}
