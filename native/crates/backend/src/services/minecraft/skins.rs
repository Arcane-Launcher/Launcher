use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::models::minecraft::{AuthSession, SessionType};
use crate::paths::{sanitize_component, AppPaths};
use crate::services::cache::store::now_secs;
use crate::services::minecraft::mineskin::{self, MineSkinRef};
use crate::services::network::connectivity::prefer_ipv4;

#[derive(Debug, Clone, Serialize)]
pub struct SkinInfo {
    pub url: Option<String>,
    pub variant: String,
}

#[derive(Deserialize)]
struct ProfileResponse {
    #[serde(default)]
    properties: Vec<ProfileProperty>,
}

#[derive(Deserialize)]
struct ProfileProperty {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct TexturesPayload {
    textures: Textures,
}

#[derive(Deserialize, Default)]
struct Textures {
    #[serde(rename = "SKIN", default)]
    skin: Option<SkinTexture>,
}

#[derive(Deserialize)]
struct SkinTexture {
    url: String,
    #[serde(default)]
    metadata: Option<SkinMetadata>,
}

#[derive(Deserialize)]
struct SkinMetadata {
    model: Option<String>,
}

fn client() -> reqwest::Client {
    prefer_ipv4(reqwest::Client::builder().user_agent("ArcaneLauncher/0.1"))
        .build()
        .expect("http client")
}

/// Reads a profile's current skin straight from Mojang's public session
/// server — no auth needed, works for any player (used to render the "your
/// current skin" preview).
pub async fn get_skin(profile_id: &str) -> AppResult<SkinInfo> {
    let url = format!("https://sessionserver.mojang.com/session/minecraft/profile/{profile_id}");
    let res = client().get(&url).send().await?;
    if !res.status().is_success() {
        return Err(AppError::new(
            "Couldn't load this profile's skin.",
            format!("{} from {url}", res.status()),
        ));
    }
    let body: ProfileResponse = res.json().await?;
    let Some(prop) = body.properties.into_iter().find(|p| p.name == "textures") else {
        return Ok(SkinInfo {
            url: None,
            variant: "classic".to_string(),
        });
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(prop.value)
        .map_err(|e| AppError::new("Couldn't read this profile's skin data.", e.to_string()))?;
    let payload: TexturesPayload = serde_json::from_slice(&decoded)?;
    Ok(match payload.textures.skin {
        Some(skin) => {
            let is_slim = skin.metadata.and_then(|m| m.model).as_deref() == Some("slim");
            SkinInfo {
                url: Some(skin.url),
                variant: if is_slim { "slim" } else { "classic" }.to_string(),
            }
        }
        None => SkinInfo {
            url: None,
            variant: "classic".to_string(),
        },
    })
}

fn require_token(session: &AuthSession) -> AppResult<&str> {
    session.access_token.as_deref().ok_or_else(|| {
        AppError::new(
            "Sign in with a Microsoft account to manage your skin.",
            "no minecraft access token on session",
        )
    })
}

/// Turns a failed Mojang API response into an `AppError`, keeping the
/// response body in the technical detail so failures are diagnosable.
async fn auth_failure(res: reqwest::Response, context: &str) -> AppError {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let technical = format!("{status} from {context}: {body}");
    match status.as_u16() {
        401 => AppError::new(
            "Your session has expired. Sign in again to continue.",
            technical,
        ),
        429 => AppError::retryable(
            "Mojang is rate-limiting requests. Wait a minute and try again.",
            technical,
        ),
        _ => AppError::new(format!("Couldn't {context}."), technical),
    }
}

/// Uploads a new skin PNG for the signed-in Microsoft account. `variant` is
/// `"classic"` (Steve arms) or `"slim"` (Alex arms) — anything else is
/// treated as classic.
pub async fn upload_skin(session: &AuthSession, file_bytes: Vec<u8>, variant: &str) -> AppResult<()> {
    let token = require_token(session)?;
    let variant = if variant == "slim" { "slim" } else { "classic" };
    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name("skin.png")
        .mime_str("image/png")
        .map_err(|e| AppError::new("Couldn't prepare that image.", e.to_string()))?;
    let form = reqwest::multipart::Form::new()
        .text("variant", variant.to_string())
        .part("file", part);

    let res = client()
        .post("https://api.minecraftservices.com/minecraft/profile/skins")
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(auth_failure(res, "update your skin").await);
    }
    Ok(())
}

/// Resets the account back to its default Steve/Alex skin.
pub async fn reset_skin(session: &AuthSession) -> AppResult<()> {
    let token = require_token(session)?;
    let res = client()
        .delete("https://api.minecraftservices.com/minecraft/profile/skins/active")
        .bearer_auth(token)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(auth_failure(res, "reset your skin").await);
    }
    Ok(())
}

// --- Local skin library -------------------------------------------------
//
// A small per-profile library of skins the user has added in the launcher,
// independent of Mojang's own single-skin-per-account model. Selecting one:
//   - for a Microsoft session, genuinely uploads it via `upload_skin` so it
//     takes effect in the real game, not just the launcher's own preview;
//   - for an offline session, there is no real account to push to (Mojang's
//     skin API requires a Microsoft access token, and vanilla Minecraft
//     only ever renders Steve/Alex for offline play) — so this can only
//     update the launcher's own local preview. Making a custom skin
//     actually appear in game for an offline account would require
//     replacing Minecraft's whole auth flow with a local
//     Yggdrasil-compatible server (the `authlib-injector` technique real
//     "cracked" launchers use) — a much larger, separate feature.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSkin {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub variant: String,
    pub added_at: u64,
    #[serde(default)]
    pub favorite: bool,
    /// Set once this skin has been uploaded to MineSkin (offline accounts
    /// only) — cached so re-selecting it never uploads twice.
    #[serde(default)]
    pub mineskin: Option<MineSkinRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedSkinWithData {
    pub id: String,
    pub name: Option<String>,
    pub variant: String,
    pub added_at: u64,
    pub favorite: bool,
    pub mineskin: Option<MineSkinRef>,
    pub data_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkinLibrary {
    pub skins: Vec<SavedSkinWithData>,
    pub active_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SkinManifest {
    #[serde(default)]
    skins: Vec<SavedSkin>,
    #[serde(default)]
    active_id: Option<String>,
}

fn profile_dir(paths: &AppPaths, profile_id: &str) -> std::path::PathBuf {
    paths.skins_dir().join(sanitize_component(profile_id))
}

fn manifest_path(paths: &AppPaths, profile_id: &str) -> std::path::PathBuf {
    profile_dir(paths, profile_id).join("manifest.json")
}

fn skin_file_path(paths: &AppPaths, profile_id: &str, skin_id: &str) -> std::path::PathBuf {
    profile_dir(paths, profile_id).join(format!("{}.png", sanitize_component(skin_id)))
}

async fn read_manifest(paths: &AppPaths, profile_id: &str) -> SkinManifest {
    match tokio::fs::read(manifest_path(paths, profile_id)).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => SkinManifest::default(),
    }
}

async fn write_manifest(paths: &AppPaths, profile_id: &str, manifest: &SkinManifest) -> AppResult<()> {
    tokio::fs::create_dir_all(profile_dir(paths, profile_id)).await?;
    let json = serde_json::to_vec_pretty(manifest)?;
    tokio::fs::write(manifest_path(paths, profile_id), json).await?;
    Ok(())
}

/// Reads a PNG's width/height straight from its IHDR chunk (the first chunk
/// after the fixed 8-byte signature) — no image-decoding dependency needed
/// just to validate dimensions.
fn read_png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[0..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

/// Mojang texture hash of the active saved skin, once it's been uploaded
/// to MineSkin (offline accounts) — lets head renderers show it by hash.
pub async fn active_texture_hash(paths: &AppPaths, profile_id: &str) -> Option<String> {
    let manifest = read_manifest(paths, profile_id).await;
    let active = manifest.active_id.as_deref()?;
    let skin = manifest.skins.iter().find(|s| s.id == active)?;
    let hash = skin.mineskin.as_ref()?.url.rsplit('/').next()?;
    (!hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| hash.to_string())
}

/// Lists every skin saved in this profile's library, each with its full
/// image data inlined as a data URL (skins are only a few KB, so this
/// avoids a separate round trip per thumbnail).
pub async fn list_saved_skins(paths: &AppPaths, profile_id: &str) -> SkinLibrary {
    let manifest = read_manifest(paths, profile_id).await;
    let mut skins = Vec::with_capacity(manifest.skins.len());
    for skin in manifest.skins {
        if let Ok(bytes) = tokio::fs::read(skin_file_path(paths, profile_id, &skin.id)).await {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            skins.push(SavedSkinWithData {
                id: skin.id,
                name: skin.name,
                variant: skin.variant,
                added_at: skin.added_at,
                favorite: skin.favorite,
                mineskin: skin.mineskin,
                data_url: format!("data:image/png;base64,{encoded}"),
            });
        }
    }
    SkinLibrary {
        skins,
        active_id: manifest.active_id,
    }
}

/// Adds a new skin to this profile's library (without selecting it).
/// `variant` is `"classic"` (Steve arms) or `"slim"` (Alex arms) — anything
/// else is treated as classic.
pub async fn add_saved_skin(
    paths: &AppPaths,
    profile_id: &str,
    file_bytes: Vec<u8>,
    variant: &str,
    name: Option<String>,
) -> AppResult<SavedSkin> {
    let (width, height) = read_png_dimensions(&file_bytes)
        .ok_or_else(|| AppError::new("That doesn't look like a valid PNG image.", "failed to read PNG IHDR chunk"))?;
    if !((width == 64 && height == 64) || (width == 64 && height == 32)) {
        return Err(AppError::new(
            "Skins must be a 64×64 (or legacy 64×32) PNG.",
            format!("got {width}x{height}"),
        ));
    }
    let variant = if variant == "slim" { "slim" } else { "classic" };
    let id = uuid::Uuid::new_v4().simple().to_string();
    tokio::fs::create_dir_all(profile_dir(paths, profile_id)).await?;
    tokio::fs::write(skin_file_path(paths, profile_id, &id), &file_bytes).await?;

    let saved = SavedSkin {
        id,
        name: name.filter(|n| !n.trim().is_empty()),
        variant: variant.to_string(),
        added_at: now_secs(),
        favorite: false,
        mineskin: None,
    };
    let mut manifest = read_manifest(paths, profile_id).await;
    manifest.skins.push(saved.clone());
    write_manifest(paths, profile_id, &manifest).await?;
    Ok(saved)
}

/// Downloads a skin texture from an arbitrary URL (an account skin's own
/// texture, a default skin, anything already-hosted) and saves it into this
/// profile's local library — the shared plumbing behind both "save this
/// account skin locally" and picking a default skin.
pub async fn add_skin_from_url(
    paths: &AppPaths,
    profile_id: &str,
    url: &str,
    variant: &str,
    name: Option<String>,
) -> AppResult<SavedSkin> {
    let bytes = client().get(url).send().await?.bytes().await?.to_vec();
    add_saved_skin(paths, profile_id, bytes, variant, name).await
}

/// Renames a saved skin (or clears its name if `name` is `None`/blank).
pub async fn rename_saved_skin(paths: &AppPaths, profile_id: &str, skin_id: &str, name: Option<String>) -> AppResult<()> {
    let mut manifest = read_manifest(paths, profile_id).await;
    let Some(entry) = manifest.skins.iter_mut().find(|s| s.id == skin_id) else {
        return Err(AppError::not_found("This skin"));
    };
    entry.name = name.filter(|n| !n.trim().is_empty());
    write_manifest(paths, profile_id, &manifest).await
}

pub async fn set_skin_favorite(paths: &AppPaths, profile_id: &str, skin_id: &str, favorite: bool) -> AppResult<()> {
    let mut manifest = read_manifest(paths, profile_id).await;
    let Some(entry) = manifest.skins.iter_mut().find(|s| s.id == skin_id) else {
        return Err(AppError::not_found("This skin"));
    };
    entry.favorite = favorite;
    write_manifest(paths, profile_id, &manifest).await
}

/// Writes a saved skin's raw PNG bytes to an arbitrary destination path
/// (chosen via a save dialog on the frontend) so it can be shared or backed
/// up outside the launcher.
pub async fn export_saved_skin(paths: &AppPaths, profile_id: &str, skin_id: &str, dest_path: &str) -> AppResult<()> {
    let bytes = tokio::fs::read(skin_file_path(paths, profile_id, skin_id))
        .await
        .map_err(|_| AppError::not_found("This skin"))?;
    tokio::fs::write(dest_path, bytes)
        .await
        .map_err(|e| AppError::new("Couldn't save that file.", format!("{e} ({dest_path})")))
}

#[derive(Deserialize)]
struct UsernameLookup {
    id: String,
}

/// Resolves a Java-edition username to its account via Mojang's public
/// username-lookup API, reads that account's current skin the same way
/// [`get_skin`] does, downloads the texture, and saves it into this
/// profile's local library under the looked-up username. Works for any
/// player and needs no authentication.
pub async fn import_skin_by_username(paths: &AppPaths, profile_id: &str, username: &str) -> AppResult<SavedSkin> {
    let lookup_url = format!("https://api.mojang.com/users/profiles/minecraft/{username}");
    let res = client().get(&lookup_url).send().await?;
    if res.status().as_u16() == 404 {
        return Err(AppError::new(
            format!("No player named \"{username}\" exists."),
            format!("404 from {lookup_url}"),
        ));
    }
    if !res.status().is_success() {
        return Err(AppError::new(
            "Couldn't look up that username.",
            format!("{} from {lookup_url}", res.status()),
        ));
    }
    let found: UsernameLookup = res.json().await?;
    let info = get_skin(&found.id).await?;
    let Some(skin_url) = info.url else {
        return Err(AppError::new(
            format!("\"{username}\" doesn't have a custom skin."),
            "no SKIN texture on profile",
        ));
    };
    let bytes = client().get(&skin_url).send().await?.bytes().await?.to_vec();
    add_saved_skin(paths, profile_id, bytes, &info.variant, Some(username.to_string())).await
}

/// Removes a skin from the library. If it was the active one, the account
/// falls back to its real default (nothing further to do here — the
/// frontend is expected to call `clear_active_skin` first if it wants that
/// to also reset the Microsoft account's live skin).
pub async fn delete_saved_skin(paths: &AppPaths, profile_id: &str, skin_id: &str) -> AppResult<()> {
    let _ = tokio::fs::remove_file(skin_file_path(paths, profile_id, skin_id)).await;
    let mut manifest = read_manifest(paths, profile_id).await;
    manifest.skins.retain(|s| s.id != skin_id);
    if manifest.active_id.as_deref() == Some(skin_id) {
        manifest.active_id = None;
    }
    write_manifest(paths, profile_id, &manifest).await
}

/// Marks a saved skin as this profile's active one. For a Microsoft session
/// this genuinely uploads it to Mojang; for an offline session it only
/// updates the local preview, since there is no real account to push to.
pub async fn select_saved_skin(
    paths: &AppPaths,
    profile_id: &str,
    skin_id: &str,
    session: Option<&AuthSession>,
) -> AppResult<()> {
    let mut manifest = read_manifest(paths, profile_id).await;
    let Some(entry) = manifest.skins.iter().find(|s| s.id == skin_id) else {
        return Err(AppError::not_found("This skin"));
    };
    if let Some(session) = session {
        if session.session_type == SessionType::Microsoft {
            let variant = entry.variant.clone();
            let bytes = tokio::fs::read(skin_file_path(paths, profile_id, skin_id)).await?;
            upload_skin(session, bytes, &variant).await?;
        }
    }
    manifest.active_id = Some(skin_id.to_string());
    write_manifest(paths, profile_id, &manifest).await
}

/// Uploads a saved skin to MineSkin so servers running SkinsRestorer can
/// apply it (offline accounts only — the caller enforces that). The result
/// is cached on the skin, so this is a no-op returning the cached data
/// unless `force` is set.
pub async fn sync_to_mineskin(
    paths: &AppPaths,
    profile_id: &str,
    skin_id: &str,
    force: bool,
) -> AppResult<MineSkinRef> {
    let manifest = read_manifest(paths, profile_id).await;
    let Some(entry) = manifest.skins.iter().find(|s| s.id == skin_id) else {
        return Err(AppError::not_found("This skin"));
    };
    if let (Some(cached), false) = (&entry.mineskin, force) {
        return Ok(cached.clone());
    }
    let variant = entry.variant.clone();
    let name = entry.name.clone();
    let bytes = tokio::fs::read(skin_file_path(paths, profile_id, skin_id))
        .await
        .map_err(|_| AppError::not_found("This skin"))?;

    let uploaded = mineskin::generate(bytes, &variant, name.as_deref(), now_secs()).await?;

    // Re-read before writing: the manifest may have changed during the upload
    // (which can take a while), and the skin may even have been deleted.
    let mut manifest = read_manifest(paths, profile_id).await;
    if let Some(entry) = manifest.skins.iter_mut().find(|s| s.id == skin_id) {
        entry.mineskin = Some(uploaded.clone());
        write_manifest(paths, profile_id, &manifest).await?;
    }
    Ok(uploaded)
}

/// Clears the active saved skin, falling back to the account's real default
/// (Mojang's Steve/Alex for a Microsoft session; the hash-based default for
/// an offline one).
pub async fn clear_active_skin(paths: &AppPaths, profile_id: &str, session: Option<&AuthSession>) -> AppResult<()> {
    if let Some(session) = session {
        if session.session_type == SessionType::Microsoft {
            reset_skin(session).await?;
        }
    }
    let mut manifest = read_manifest(paths, profile_id).await;
    manifest.active_id = None;
    write_manifest(paths, profile_id, &manifest).await
}

// --- Capes ---------------------------------------------------------------
//
// Capes are a separate cosmetic slot from skins in Mojang's own model: an
// account can *own* several (migrator/vanilla/event/cape-store capes) but
// only ever has one active at a time, and — unlike skins — there is no
// upload; you can only equip one you already own. Real, documented
// endpoints on api.minecraftservices.com (verified against the same API
// family as the skin upload/reset calls above), Microsoft accounts only.

#[derive(Debug, Clone, Serialize)]
pub struct CapeInfo {
    pub id: String,
    pub alias: String,
    pub url: String,
    pub active: bool,
}

#[derive(Deserialize, Default)]
struct ProfileCapesResponse {
    #[serde(default)]
    capes: Vec<RawCape>,
    #[serde(default)]
    skins: Vec<RawAccountSkin>,
}

#[derive(Deserialize)]
struct RawCape {
    id: String,
    state: String,
    url: String,
    #[serde(default)]
    alias: String,
}

#[derive(Deserialize)]
struct RawAccountSkin {
    id: String,
    state: String,
    url: String,
    #[serde(default)]
    variant: String,
}

/// One of the skins Mojang has on file for this Microsoft account —
/// distinct from Arcane's own local skin library, this is the account's
/// real skin history as `GET /minecraft/profile` reports it.
#[derive(Debug, Clone, Serialize)]
pub struct AccountSkinInfo {
    pub id: String,
    pub url: String,
    pub variant: String,
    pub active: bool,
}

/// Lists every skin Mojang has on record for this Microsoft account (its
/// real skin history, not Arcane's local library) — the same `/minecraft/
/// profile` endpoint used for capes, just reading its `skins` array
/// instead.
pub async fn list_account_skins(session: &AuthSession) -> AppResult<Vec<AccountSkinInfo>> {
    let token = require_token(session)?;
    let res = client()
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(token)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(auth_failure(res, "load your account's skins").await);
    }
    let body: ProfileCapesResponse = res.json().await?;
    Ok(body
        .skins
        .into_iter()
        .map(|s| AccountSkinInfo {
            active: s.state == "ACTIVE",
            id: s.id,
            url: s.url,
            variant: if s.variant.eq_ignore_ascii_case("slim") {
                "slim".to_string()
            } else {
                "classic".to_string()
            },
        })
        .collect())
}

/// Lists every cape this Microsoft account owns, alongside which one (if
/// any) is currently equipped.
pub async fn list_owned_capes(session: &AuthSession) -> AppResult<Vec<CapeInfo>> {
    let token = require_token(session)?;
    let res = client()
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(token)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(auth_failure(res, "load your capes").await);
    }
    let body: ProfileCapesResponse = res.json().await?;
    Ok(body
        .capes
        .into_iter()
        .map(|c| CapeInfo {
            active: c.state == "ACTIVE",
            id: c.id,
            alias: if c.alias.trim().is_empty() { "Cape".to_string() } else { c.alias },
            url: c.url,
        })
        .collect())
}

#[derive(Serialize)]
struct SetCapeBody<'a> {
    #[serde(rename = "capeId")]
    cape_id: &'a str,
}

/// Equips one of the account's already-owned capes.
pub async fn set_active_cape(session: &AuthSession, cape_id: &str) -> AppResult<()> {
    let token = require_token(session)?;
    let res = client()
        .put("https://api.minecraftservices.com/minecraft/profile/capes/active")
        .bearer_auth(token)
        .json(&SetCapeBody { cape_id })
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(auth_failure(res, "equip that cape").await);
    }
    Ok(())
}

/// Unequips whatever cape is active, without giving up ownership of it.
pub async fn clear_active_cape(session: &AuthSession) -> AppResult<()> {
    let token = require_token(session)?;
    let res = client()
        .delete("https://api.minecraftservices.com/minecraft/profile/capes/active")
        .bearer_auth(token)
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(auth_failure(res, "remove your cape").await);
    }
    Ok(())
}
