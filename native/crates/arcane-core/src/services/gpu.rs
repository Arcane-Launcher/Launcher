//! Graphics card detection and "which GPU does Minecraft use".
//!
//! On a laptop with integrated + dedicated graphics, Windows runs `javaw.exe`
//! on the power-saving (integrated) GPU unless told otherwise — the most
//! common reason Minecraft runs badly on a gaming laptop. Windows keeps that
//! choice per executable in
//! `HKCU\Software\Microsoft\DirectX\UserGpuPreferences` (the same value
//! Settings > System > Display > Graphics writes), so the launcher sets it
//! for the Java it's about to start. On Linux the equivalent is PRIME render
//! offload, requested through environment variables on the game process.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::models::settings::GpuPreference;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuRole {
    /// The only graphics card: nothing to choose.
    Only,
    /// What Windows picks for "High performance".
    HighPerformance,
    /// What Windows picks for "Power saving".
    PowerSaving,
    /// Present, but Windows ranks it first for neither (or the platform
    /// doesn't say).
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: Option<String>,
    /// Video memory of its own; `None` where the platform doesn't report it.
    pub dedicated_memory_mb: Option<u64>,
    pub role: GpuRole,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuReport {
    pub gpus: Vec<GpuInfo>,
    /// Whether the GPU preference setting does anything on this PC.
    pub can_choose: bool,
    /// Why it doesn't, when it doesn't.
    pub note: Option<String>,
}

pub async fn detect() -> GpuReport {
    tokio::task::spawn_blocking(detect_blocking)
        .await
        .unwrap_or_else(|_| GpuReport {
            gpus: Vec::new(),
            can_choose: false,
            note: Some("Graphics cards couldn't be detected.".into()),
        })
}

fn vendor_name(vendor_id: u32) -> Option<&'static str> {
    match vendor_id {
        0x10DE => Some("NVIDIA"),
        0x1002 | 0x1022 => Some("AMD"),
        0x8086 => Some("Intel"),
        0x5143 => Some("Qualcomm"),
        0x106B => Some("Apple"),
        _ => None,
    }
}

#[cfg(windows)]
fn detect_blocking() -> GpuReport {
    let gpus = windows_impl::list().unwrap_or_default();
    let can_choose = gpus.iter().any(|g| g.role == GpuRole::HighPerformance)
        && gpus.iter().any(|g| g.role == GpuRole::PowerSaving);
    let note = match gpus.len() {
        0 => Some("Graphics cards couldn't be detected.".into()),
        1 => Some("This PC has one graphics card, so Minecraft always uses it.".into()),
        _ if !can_choose => Some("Windows ranks the same graphics card first for both performance and power saving, so there's nothing to switch.".into()),
        _ => None,
    };
    GpuReport { gpus, can_choose, note }
}

#[cfg(target_os = "linux")]
fn detect_blocking() -> GpuReport {
    let output = std::process::Command::new("lspci").output();
    let names: Vec<String> = output
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("vga compatible") || l.contains("3d controller") || l.contains("display controller")
        })
        .filter_map(|l| l.splitn(3, ':').nth(2).map(|n| n.trim().to_string()))
        .filter(|n| !n.is_empty())
        .collect();
    let only = names.len() == 1;
    let gpus: Vec<GpuInfo> = names
        .into_iter()
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            let vendor = ["NVIDIA", "AMD", "Intel"]
                .into_iter()
                .find(|v| lower.contains(&v.to_ascii_lowercase()) || (*v == "AMD" && lower.contains("ati ")))
                .map(str::to_string);
            GpuInfo {
                name,
                vendor,
                dedicated_memory_mb: None,
                role: if only { GpuRole::Only } else { GpuRole::Other },
            }
        })
        .collect();
    let can_choose = gpus.len() >= 2;
    let note = match gpus.len() {
        0 => Some("Graphics cards couldn't be detected (lspci isn't available).".into()),
        1 => Some("This PC has one graphics card, so Minecraft always uses it.".into()),
        _ => None,
    };
    GpuReport { gpus, can_choose, note }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn detect_blocking() -> GpuReport {
    GpuReport {
        gpus: Vec::new(),
        can_choose: false,
        note: Some("macOS switches graphics automatically; apps can't choose a GPU.".into()),
    }
}

/// Environment variables for the game process (Linux PRIME offload). Empty
/// on Windows, where the choice lives in the registry instead.
pub fn launch_env(preference: GpuPreference) -> Vec<(String, String)> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
    match preference {
        GpuPreference::Auto => Vec::new(),
        GpuPreference::PowerSaving => vec![pair("DRI_PRIME", "0")],
        GpuPreference::HighPerformance => {
            let mut env = vec![pair("DRI_PRIME", "1")];
            // NVIDIA's proprietary driver ignores DRI_PRIME and uses its own
            // offload switches — only valid when that driver is loaded.
            if Path::new("/proc/driver/nvidia/version").exists() {
                env.push(pair("__NV_PRIME_RENDER_OFFLOAD", "1"));
                env.push(pair("__GLX_VENDOR_LIBRARY_NAME", "nvidia"));
                env.push(pair("__VK_LAYER_NV_optimus", "NVIDIA_only"));
            }
            env
        }
    }
}

/// Java executables whose Windows GPU preference the launcher has set, so
/// "Auto" removes only what the launcher wrote (never a choice the player
/// made themselves in Windows Settings).
fn applied_file(root: &Path) -> PathBuf {
    root.join("gpu-preference-applied.json")
}

fn load_applied(root: &Path) -> Vec<String> {
    std::fs::read(applied_file(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_applied(root: &Path, list: &[String]) {
    if let Ok(json) = serde_json::to_vec_pretty(list) {
        if let Err(e) = std::fs::write(applied_file(root), json) {
            log::warn!("couldn't record GPU preference paths: {e}");
        }
    }
}

/// Windows' own value format is `Key=Value;` pairs (other keys such as
/// `SwapEffectUpgradeEnable` can share the value). Returns the new value
/// with `GpuPreference` set to `pref`, or removed when `pref` is `None`.
fn with_gpu_preference(existing: &str, pref: Option<u8>) -> String {
    let mut out: String = existing
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.to_ascii_lowercase().starts_with("gpupreference="))
        .map(|p| format!("{p};"))
        .collect();
    if let Some(n) = pref {
        out.push_str(&format!("GpuPreference={n};"));
    }
    out
}

fn windows_value(preference: GpuPreference) -> Option<u8> {
    match preference {
        GpuPreference::Auto => None,
        GpuPreference::PowerSaving => Some(1),
        GpuPreference::HighPerformance => Some(2),
    }
}

/// Makes the Java about to be launched follow `preference`. Best-effort: a
/// failure is logged and the game still starts.
pub fn apply_for_launch(root: &Path, preference: GpuPreference, java_path: &str) {
    if !cfg!(windows) {
        return;
    }
    let exe = std::path::absolute(java_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| java_path.to_string());
    let mut applied = load_applied(root);
    let known = applied.iter().any(|p| p.eq_ignore_ascii_case(&exe));
    match preference {
        GpuPreference::Auto if !known => {}
        GpuPreference::Auto => {
            if write_registry(&exe, None) {
                applied.retain(|p| !p.eq_ignore_ascii_case(&exe));
                save_applied(root, &applied);
            }
        }
        _ => {
            if write_registry(&exe, windows_value(preference)) && !known {
                applied.push(exe);
                save_applied(root, &applied);
            }
        }
    }
}

/// Re-applies a changed setting right away to every Java the launcher set a
/// preference for, so switching back to Auto takes effect without a launch.
pub fn reapply_recorded(root: &Path, preference: GpuPreference) {
    if !cfg!(windows) {
        return;
    }
    let applied = load_applied(root);
    let value = windows_value(preference);
    for exe in &applied {
        write_registry(exe, value);
    }
    if preference == GpuPreference::Auto && !applied.is_empty() {
        save_applied(root, &[]);
    }
}

#[cfg(windows)]
fn write_registry(exe: &str, pref: Option<u8>) -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
    let key = match RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) {
        Ok((key, _)) => key,
        Err(e) => {
            log::warn!("couldn't open {KEY}: {e}");
            return false;
        }
    };
    let existing: String = key.get_value(exe).unwrap_or_default();
    let value = with_gpu_preference(&existing, pref);
    let result = if value.is_empty() {
        match key.delete_value(exe) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    } else {
        key.set_value(exe, &value)
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            log::warn!("couldn't set GPU preference for {exe}: {e}");
            false
        }
    }
}

#[cfg(not(windows))]
fn write_registry(_exe: &str, _pref: Option<u8>) -> bool {
    false
}

#[cfg(windows)]
mod windows_impl {
    use super::{vendor_name, GpuInfo, GpuRole};
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory6, DXGI_ADAPTER_DESC1,
        DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
        DXGI_GPU_PREFERENCE_MINIMUM_POWER,
    };

    fn luid(desc: &DXGI_ADAPTER_DESC1) -> (u32, i32) {
        (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart)
    }

    fn is_hardware(desc: &DXGI_ADAPTER_DESC1) -> bool {
        desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0
            // The "Microsoft Basic Render Driver" isn't always flagged.
            && desc.VendorId != 0x1414
    }

    fn ordered(factory: &IDXGIFactory6, preference: DXGI_GPU_PREFERENCE) -> Vec<DXGI_ADAPTER_DESC1> {
        (0..16)
            .map_while(|i| unsafe { factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, preference) }.ok())
            .filter_map(|a| unsafe { a.GetDesc1() }.ok())
            .filter(is_hardware)
            .collect()
    }

    pub fn list() -> Option<Vec<GpuInfo>> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;

        // Windows 10 1803+: the exact ranking the Graphics settings page uses.
        let (adapters, high, low) = if let Ok(f6) = factory.cast::<IDXGIFactory6>() {
            let high = ordered(&f6, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE);
            let low = ordered(&f6, DXGI_GPU_PREFERENCE_MINIMUM_POWER);
            let (h, l) = (high.first().map(luid), low.first().map(luid));
            (high, h, l)
        } else {
            let all: Vec<DXGI_ADAPTER_DESC1> = (0..16)
                .map_while(|i| unsafe { factory.EnumAdapters1(i) }.ok())
                .filter_map(|a| unsafe { a.GetDesc1() }.ok())
                .filter(is_hardware)
                .collect();
            (all, None, None)
        };

        let only = adapters.len() == 1;
        Some(
            adapters
                .iter()
                .map(|desc| {
                    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                    let id = Some(luid(desc));
                    let role = if only {
                        GpuRole::Only
                    } else if id == high && id != low {
                        GpuRole::HighPerformance
                    } else if id == low && id != high {
                        GpuRole::PowerSaving
                    } else {
                        GpuRole::Other
                    };
                    GpuInfo {
                        name: String::from_utf16_lossy(&desc.Description[..len]).trim().to_string(),
                        vendor: vendor_name(desc.VendorId).map(str::to_string),
                        dedicated_memory_mb: Some(desc.DedicatedVideoMemory as u64 / (1024 * 1024)),
                        role,
                    }
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_preference_value_keeps_other_windows_keys() {
        assert_eq!(with_gpu_preference("", Some(2)), "GpuPreference=2;");
        assert_eq!(with_gpu_preference("GpuPreference=1;", Some(2)), "GpuPreference=2;");
        assert_eq!(
            with_gpu_preference("SwapEffectUpgradeEnable=1;GpuPreference=1;", Some(2)),
            "SwapEffectUpgradeEnable=1;GpuPreference=2;"
        );
        assert_eq!(with_gpu_preference("SwapEffectUpgradeEnable=1;GpuPreference=2;", None), "SwapEffectUpgradeEnable=1;");
        assert_eq!(with_gpu_preference("GpuPreference=2;", None), "");
    }

    #[test]
    fn auto_adds_no_linux_env() {
        assert!(launch_env(GpuPreference::Auto).is_empty());
    }

    #[test]
    fn detects_without_panicking() {
        let report = detect_blocking();
        eprintln!("{report:#?}");
        assert_eq!(report.can_choose, report.note.is_none());
    }
}
