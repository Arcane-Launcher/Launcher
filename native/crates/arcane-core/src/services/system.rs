use serde::Serialize;
use sysinfo::System;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SystemMemoryInfo {
    pub total_mb: u64,
}

pub fn total_memory_mb() -> u64 {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.total_memory() / (1024 * 1024)
}

/// Physical memory the OS could hand out right now (free + reclaimable
/// cache) — what a newly started game can actually grow into without the
/// rest of the system being pushed into the page file.
pub fn available_memory_mb() -> u64 {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.available_memory() / (1024 * 1024)
}

pub fn get_system_memory() -> SystemMemoryInfo {
    SystemMemoryInfo {
        total_mb: total_memory_mb(),
    }
}

/// Best-effort GPU name detection for the Smart Launch health check —
/// there's no cross-platform crate already in this project for adapter
/// enumeration, so this shells out to each OS's own tooling rather than
/// pulling in a new graphics dependency just for a display name. Returns
/// `None` (never a guess) if the platform isn't covered or the command
/// fails/times out, since an honest "couldn't detect" beats a fabricated
/// answer.
pub async fn detect_gpu_name() -> Option<String> {
    // Windows reads the display adapter class key directly. The old hidden
    // PowerShell call is exactly the kind of thing antivirus heuristics flag
    // in an unsigned app.
    #[cfg(target_os = "windows")]
    {
        windows_gpu_name()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        if cfg!(target_os = "macos") {
            cmd.args([
                "-c",
                "system_profiler SPDisplaysDataType | grep 'Chipset Model' | head -1 | cut -d: -f2",
            ]);
        } else {
            cmd.args(["-c", "lspci | grep -i 'vga\\|3d controller' | head -1 | cut -d: -f3"]);
        }

        crate::services::process_ext::suppress_console_window(&mut cmd);
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output())
            .await
            .ok()?
            .ok()?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() { None } else { Some(name) }
    }
}

/// The first display adapter's name from the Display device class key
/// (`0000`, `0001`, …, each with a `DriverDesc`) — the same name Device
/// Manager and Win32_VideoController show. The generic "Microsoft Basic
/// Display Adapter" is only used when nothing else is installed.
#[cfg(target_os = "windows")]
fn windows_gpu_name() -> Option<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    const DISPLAY_CLASS: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let class = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(DISPLAY_CLASS, KEY_READ)
        .ok()?;

    let mut subkeys: Vec<String> = class
        .enum_keys()
        .filter_map(Result::ok)
        .filter(|k| k.chars().all(|c| c.is_ascii_digit()))
        .collect();
    subkeys.sort();

    let names: Vec<String> = subkeys
        .iter()
        .filter_map(|k| class.open_subkey(k).ok()?.get_value::<String, _>("DriverDesc").ok())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();

    names
        .iter()
        .find(|n| !n.contains("Basic Display") && !n.contains("Basic Render"))
        .or(names.first())
        .cloned()
}

/// A reasonable default JVM heap for a freshly created instance: a quarter
/// of system RAM, clamped to a sane range so a 4GB machine doesn't get
/// starved and a 128GB workstation doesn't over-allocate for no reason.
pub fn default_instance_memory_mb() -> u32 {
    let total = total_memory_mb();
    let quarter = total / 4;
    quarter.clamp(1024, 8192) as u32
}
