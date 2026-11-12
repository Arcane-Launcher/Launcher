use crate::error::AppResult;
use crate::models::minecraft::{AssetIndex, VersionDetail};
use crate::paths::AppPaths;
use crate::services::download::manager::{
    DownloadItem, DownloadManager, ExpectedHash, MAX_PARALLEL_DOWNLOADS,
};

/// Asset indexes whose deferred (music/language) batch is downloading in
/// the background right now, so launching twice never starts it twice.
static DEFERRED_IN_FLIGHT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Assets the game can start without. Measured on the 26.3 index: music
/// (259 MB) + music discs (34 MB) + non-English languages (90 MB) are 383
/// of its 484 MB. Minecraft skips a sound whose file is missing (it logs
/// "does not exist, cannot add it to event") and English text ships inside
/// the client jar, so these can arrive after the game is already playable.
/// Only modern `minecraft/`-namespaced keys qualify; legacy (pre-1.7)
/// indexes use a different layout and always download in full.
fn is_deferrable(key: &str) -> bool {
    key.starts_with("minecraft/sounds/music/")
        || key.starts_with("minecraft/sounds/records/")
        || (key.starts_with("minecraft/lang/") && key != "minecraft/lang/en_us.json")
}

/// Downloads the asset index and every referenced object that isn't already
/// cached in the shared assets store. Objects are content-addressed by
/// sha1, exactly like Mojang's own launcher, so every instance on the same
/// Minecraft version shares one copy.
///
/// With `defer_optional`, only what's needed to play is awaited; music and
/// extra languages ([`is_deferrable`]) then download as a separate
/// background batch that doesn't hold up the launch.
pub async fn ensure_assets(
    version: &VersionDetail,
    paths: &AppPaths,
    downloads: &DownloadManager,
    instance_id: Option<String>,
    defer_optional: bool,
) -> AppResult<()> {
    let Some(asset_index_ref) = &version.asset_index else {
        return Ok(());
    };

    let index_path = paths
        .assets_indexes_dir()
        .join(format!("{}.json", asset_index_ref.id));

    if !index_path.exists() {
        downloads
            .download_file(
                &format!("assets ({})", asset_index_ref.id),
                &asset_index_ref.url,
                index_path.clone(),
                ExpectedHash {
                    sha1: Some(asset_index_ref.sha1.clone()),
                    sha256: None,
                    sha512: None,
                },
                instance_id.clone(),
            )
            .await?;
    }

    let bytes = tokio::fs::read(&index_path).await?;
    let index: AssetIndex = serde_json::from_slice(&bytes)?;

    let mut pending = Vec::new();
    let mut deferred = Vec::new();
    // Many names in an index can point at the same object; fetch each hash once.
    let mut queued = std::collections::HashSet::new();
    for (key, object) in &index.objects {
        let hash = &object.hash;
        if hash.len() < 2 || !queued.insert(hash.as_str()) {
            continue;
        }
        let prefix = &hash[0..2];
        let dest = paths.assets_objects_dir().join(prefix).join(hash);
        if dest.exists() {
            continue;
        }
        let item = DownloadItem {
            url: format!("https://resources.download.minecraft.net/{prefix}/{hash}"),
            dest,
            expected: ExpectedHash {
                sha1: Some(hash.clone()),
                sha256: None,
                sha512: None,
            },
            size: Some(object.size),
        };
        if defer_optional && is_deferrable(key) {
            deferred.push(item);
        } else {
            pending.push(item);
        }
    }

    downloads
        .download_many(
            &format!("Game assets ({})", asset_index_ref.id),
            pending,
            instance_id.clone(),
            MAX_PARALLEL_DOWNLOADS,
        )
        .await?;

    if !deferred.is_empty() {
        spawn_deferred(
            asset_index_ref.id.clone(),
            deferred,
            downloads.clone(),
            instance_id,
        );
    }
    Ok(())
}

/// Started only after the essential assets are done, so it never competes
/// with them for bandwidth. A failure here is logged, not surfaced: the
/// missing files are simply picked up again by the next install/launch.
fn spawn_deferred(
    index_id: String,
    items: Vec<DownloadItem>,
    downloads: DownloadManager,
    instance_id: Option<String>,
) {
    {
        let mut in_flight = DEFERRED_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if in_flight.contains(&index_id) {
            return;
        }
        in_flight.push(index_id.clone());
    }
    tokio::spawn(async move {
        let result = downloads
            .download_many(
                &format!("Music & languages ({index_id})"),
                items,
                instance_id,
                MAX_PARALLEL_DOWNLOADS,
            )
            .await;
        if let Err(e) = result {
            log::warn!(
                "background music/language download for {index_id} failed: {}",
                e.technical
            );
        }
        DEFERRED_IN_FLIGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id| id != &index_id);
    });
}

pub fn legacy_assets_needed(version: &VersionDetail) -> bool {
    version.assets.as_deref() == Some("legacy") || version.assets.as_deref() == Some("pre-1.6")
}

#[cfg(test)]
mod tests {
    use super::is_deferrable;

    #[test]
    fn only_music_discs_and_foreign_languages_are_deferred() {
        assert!(is_deferrable("minecraft/sounds/music/game/calm1.ogg"));
        assert!(is_deferrable("minecraft/sounds/records/cat.ogg"));
        assert!(is_deferrable("minecraft/lang/de_de.json"));
        assert!(!is_deferrable("minecraft/lang/en_us.json"));
        assert!(!is_deferrable("minecraft/sounds/mob/zombie/say1.ogg"));
        assert!(!is_deferrable("minecraft/sounds.json"));
        assert!(!is_deferrable("sounds/music/calm1.ogg"));
    }
}
