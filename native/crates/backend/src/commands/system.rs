use crate::services::system::{get_system_memory, SystemMemoryInfo};

pub async fn get_system_memory_info() -> Result<SystemMemoryInfo, crate::error::AppError> {
    Ok(get_system_memory())
}

pub async fn get_gpus() -> Result<crate::services::gpu::GpuReport, crate::error::AppError> {
    Ok(crate::services::gpu::detect().await)
}

/// Opens a path in the OS file manager — the instance cards' "Open
/// instance folder" button (`@tauri-apps/plugin-shell`'s `open`).
/// Fire-and-forget: `explorer.exe` returns non-zero exit codes on
/// success, so the process is only spawned, never waited on.
pub fn open_path(path: &str) -> Result<(), crate::error::AppError> {
    let result = if cfg!(windows) {
        std::process::Command::new("explorer").arg(path).spawn()
    } else {
        // xdg-open is the desktop-portal convention on Linux.
        std::process::Command::new("xdg-open").arg(path).spawn()
    };
    result
        .map(|_| ())
        .map_err(|e| crate::error::AppError::new("Couldn't open that folder.", e.to_string()))
}
