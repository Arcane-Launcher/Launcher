use base64::Engine;

use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;

/// (file extension, MIME type) for every format a custom background may be
/// stored as. Only one of these files exists on disk at a time.
const CANDIDATES: [(&str, &str); 2] = [("png", "image/png"), ("jpg", "image/jpeg")];

fn detect_image_format(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() >= 8 && bytes[0..8] == PNG_SIGNATURE {
        Some(("png", "image/png"))
    } else if bytes.len() >= 3 && bytes[0..3] == [0xFF, 0xD8, 0xFF] {
        Some(("jpg", "image/jpeg"))
    } else {
        None
    }
}

/// Returns the user's custom background as a data URL, or `None` if they
/// haven't set one — the frontend falls back to the bundled default image
/// in that case.
pub async fn get_background(paths: &AppPaths) -> Option<String> {
    for (ext, mime) in CANDIDATES {
        if let Ok(bytes) = tokio::fs::read(paths.background_file(ext)).await {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Some(format!("data:{mime};base64,{encoded}"));
        }
    }
    None
}

/// Stores a new custom background image, replacing any previous one
/// (regardless of format).
pub async fn set_background(paths: &AppPaths, file_bytes: Vec<u8>) -> AppResult<()> {
    let Some((ext, _)) = detect_image_format(&file_bytes) else {
        return Err(AppError::new(
            "That doesn't look like a valid PNG or JPEG image.",
            "unrecognized image signature",
        ));
    };
    for (other_ext, _) in CANDIDATES {
        let _ = tokio::fs::remove_file(paths.background_file(other_ext)).await;
    }
    tokio::fs::write(paths.background_file(ext), file_bytes).await?;
    Ok(())
}

/// Removes any custom background, reverting to the bundled default.
pub async fn reset_background(paths: &AppPaths) -> AppResult<()> {
    for (ext, _) in CANDIDATES {
        let _ = tokio::fs::remove_file(paths.background_file(ext)).await;
    }
    Ok(())
}
