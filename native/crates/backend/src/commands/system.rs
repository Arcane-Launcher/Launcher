use crate::services::system::{get_system_memory, SystemMemoryInfo};

pub async fn get_system_memory_info() -> Result<SystemMemoryInfo, crate::error::AppError> {
    Ok(get_system_memory())
}

pub async fn get_gpus() -> Result<crate::services::gpu::GpuReport, crate::error::AppError> {
    Ok(crate::services::gpu::detect().await)
}
