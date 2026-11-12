//! Windows implementation of the game recorder.

pub mod audio;
pub mod engine;
pub mod gpu;
pub mod remux;
pub mod sink;

/// Windows Graphics Capture of a window needs Windows 10 1903+.
pub fn is_supported() -> bool {
    use windows::core::HSTRING;
    use windows::Foundation::Metadata::ApiInformation;
    ApiInformation::IsApiContractPresentByMajor(
        &HSTRING::from("Windows.Foundation.UniversalApiContract"),
        8,
    )
    .unwrap_or(false)
        && windows::Graphics::Capture::GraphicsCaptureSession::IsSupported().unwrap_or(false)
}

/// Local wall-clock time as `2026-10-08_21.14.03`, the same shape Minecraft
/// names its screenshots with.
pub fn local_timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02}_{:02}.{:02}.{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
