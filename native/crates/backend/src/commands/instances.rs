use base64::Engine;

use crate::error::AppError;
use crate::models::common::ModLoaderKind;
use crate::models::instance::{CreateInstanceRequest, InstanceValidationReport, MinecraftInstance};
use crate::models::minecraft::VersionManifestEntry;
use crate::services::instance::validation;
use crate::state::AppState;

const MAX_ICON_BYTES: usize = 2 * 1024 * 1024;

pub async fn list_instances(state: &AppState) -> Result<Vec<MinecraftInstance>, AppError> {
    Ok(state.instances.list().await)
}

pub async fn get_instance(state: &AppState, id: String) -> Result<MinecraftInstance, AppError> {
    state.instances.get(&id).await
}

pub async fn create_instance(
    state: &AppState,
    request: CreateInstanceRequest,
) -> Result<MinecraftInstance, AppError> {
    state.instances.create(request).await
}

pub async fn update_instance(
    state: &AppState,
    instance: MinecraftInstance,
) -> Result<MinecraftInstance, AppError> {
    state.instances.update(instance).await
}

/// Sniffs the actual image format from its magic bytes rather than trusting
/// a filename extension — needed for the drag-and-drop path, where the
/// webview hands over raw bytes with no filename at all.
fn detect_icon_mime(bytes: &[u8]) -> Option<&'static str> {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() >= 8 && bytes[0..8] == PNG_SIGNATURE {
        Some("image/png")
    } else if bytes.len() >= 3 && bytes[0..3] == [0xFF, 0xD8, 0xFF] {
        Some("image/jpeg")
    } else if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

async fn apply_icon_bytes(state: &AppState, id: &str, bytes: Vec<u8>, mime: &str) -> Result<MinecraftInstance, AppError> {
    if bytes.len() > MAX_ICON_BYTES {
        return Err(AppError::new(
            "That image is too large. Pick one under 2 MB.",
            format!("icon is {} bytes", bytes.len()),
        ));
    }
    let mut instance = state.instances.get(id).await?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    instance.icon = Some(format!("data:{mime};base64,{encoded}"));
    state.instances.update(instance).await
}

/// Reads a locally-picked image file and stores it as a `data:` URI on the
/// instance, rather than a raw path — a path could point anywhere on disk
/// (and Tauri's webview asset-protocol scope wouldn't reliably serve it
/// back), while a self-contained data URI always renders and never grants
/// the frontend a filesystem path to act on.
pub async fn set_instance_icon(
    state: &AppState,
    id: String,
    file_path: String,
) -> Result<MinecraftInstance, AppError> {
    let bytes = tokio::fs::read(&file_path).await.map_err(|e| {
        AppError::new("Couldn't read that image file.", format!("{e} ({file_path})"))
    })?;
    let Some(mime) = detect_icon_mime(&bytes) else {
        return Err(AppError::new(
            "Choose a PNG, JPEG, GIF, or WebP image.",
            format!("unrecognized icon signature for {file_path}"),
        ));
    };
    apply_icon_bytes(&state, &id, bytes, mime).await
}

/// Same as `set_instance_icon`, but for an image dropped directly onto the
/// webview — those arrive as raw bytes with no filesystem path.
pub async fn set_instance_icon_bytes(
    state: &AppState,
    id: String,
    bytes: Vec<u8>,
) -> Result<MinecraftInstance, AppError> {
    let Some(mime) = detect_icon_mime(&bytes) else {
        return Err(AppError::new(
            "That doesn't look like a PNG, JPEG, GIF, or WebP image.",
            "unrecognized icon signature",
        ));
    };
    apply_icon_bytes(&state, &id, bytes, mime).await
}

pub async fn duplicate_instance(
    state: &AppState,
    id: String,
    new_name: String,
) -> Result<MinecraftInstance, AppError> {
    state.instances.duplicate(&id, new_name).await
}

/// Exports an instance's `game_dir` (mods, config, saves, resourcepacks,
/// shaderpacks, local metadata) to a `.zip` at `dest_path`, which the
/// frontend obtains via a native save dialog.
pub async fn export_instance(state: &AppState, id: String, dest_path: String) -> Result<(), AppError> {
    state.instances.export(&id, std::path::PathBuf::from(dest_path)).await
}

pub async fn delete_instance(
    state: &AppState,
    id: String,
    delete_files: bool,
) -> Result<(), AppError> {
    state.instances.delete(&id, delete_files).await
}

pub async fn validate_instance(
    state: &AppState,
    id: String,
) -> Result<InstanceValidationReport, AppError> {
    let instance = state.instances.get(&id).await?;
    let default_java = state.settings.get().await.default_java_path;
    Ok(validation::validate_instance(&instance, &state.paths, &state.engine.manifest, default_java).await)
}

pub async fn list_minecraft_versions(
    state: &AppState,
) -> Result<Vec<VersionManifestEntry>, AppError> {
    let manifest = state.engine.manifest.fetch_manifest().await?;
    Ok(manifest.versions)
}

pub async fn list_loader_versions(
    state: &AppState,
    loader: ModLoaderKind,
    minecraft_version: String,
) -> Result<Vec<String>, AppError> {
    match state.loaders.get(loader) {
        Some(loader) => loader.list_loader_versions(&minecraft_version).await,
        None => Ok(vec![]),
    }
}

pub async fn loader_install_supported(state: &AppState, loader: ModLoaderKind) -> Result<bool, AppError> {
    Ok(state.loaders.get(loader).map(|l| l.install_supported()).unwrap_or(true))
}
