//! MineSkin (mineskin.org) client. Offline accounts have no Mojang profile to
//! push a skin to, so the launcher instead has MineSkin turn the PNG into a
//! Mojang-signed texture. Servers running SkinsRestorer can then apply it
//! from the returned texture URL (`/skin url "<url>"`).
//!
//! Endpoint and shapes verified against MineSkin's OpenAPI spec
//! (https://api.mineskin.org/v2/openapi.json): `POST /v2/generate`, multipart
//! `file` + `variant`/`name`/`visibility`, `Authorization: Bearer <key>`,
//! required `User-Agent`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::services::network::connectivity::prefer_ipv4;

const GENERATE_URL: &str = "https://api.mineskin.org/v2/generate";

/// Baked in by `build.rs` (from `MINESKIN_API_KEY` or the gitignored
/// `mineskin.key`); empty when neither was present at build time.
const API_KEY: &str = env!("ARCANE_MINESKIN_API_KEY");

/// What we keep about a skin once MineSkin has processed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MineSkinRef {
    /// MineSkin's own id for the generated skin.
    pub uuid: String,
    /// Mojang-hosted texture URL — what `/skin url` is given.
    pub url: String,
    pub variant: String,
    pub uploaded_at: u64,
}

#[derive(Deserialize)]
struct GenerateResponse {
    skin: Option<GeneratedSkin>,
}

#[derive(Deserialize)]
struct GeneratedSkin {
    uuid: String,
    #[serde(default)]
    variant: String,
    texture: GeneratedTexture,
}

#[derive(Deserialize)]
struct GeneratedTexture {
    url: GeneratedUrls,
}

#[derive(Deserialize)]
struct GeneratedUrls {
    skin: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    errors: Vec<ErrorEntry>,
}

#[derive(Deserialize)]
struct ErrorEntry {
    #[serde(default)]
    message: String,
}

fn client() -> reqwest::Client {
    // Generation goes through MineSkin's queue and can take a while.
    prefer_ipv4(
        reqwest::Client::builder()
            .user_agent("ArcaneLauncher/0.1")
            .timeout(Duration::from_secs(120)),
    )
    .build()
    .expect("http client")
}

/// Uploads a skin PNG and returns its MineSkin texture data. `variant` is
/// `"classic"` or `"slim"`. Generated as `unlisted` so it never shows up in
/// MineSkin's public gallery, but stays fetchable by link.
pub async fn generate(
    bytes: Vec<u8>,
    variant: &str,
    name: Option<&str>,
    now: u64,
) -> AppResult<MineSkinRef> {
    let variant = if variant == "slim" { "slim" } else { "classic" };
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("skin.png")
        .mime_str("image/png")
        .map_err(|e| AppError::new("Couldn't prepare that image.", e.to_string()))?;
    let mut form = reqwest::multipart::Form::new()
        .text("variant", variant.to_string())
        .text("visibility", "unlisted")
        .part("file", part);
    if let Some(name) = name
        .map(|n| n.chars().take(20).collect::<String>())
        .filter(|n| !n.trim().is_empty())
    {
        form = form.text("name", name);
    }

    let mut req = client().post(GENERATE_URL).multipart(form);
    if !API_KEY.is_empty() {
        req = req.bearer_auth(API_KEY);
    }
    let res = req.send().await?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    parse_generate(status.as_u16(), &body, variant, now)
}

fn parse_generate(
    status: u16,
    body: &str,
    requested_variant: &str,
    now: u64,
) -> AppResult<MineSkinRef> {
    if status == 429 {
        return Err(AppError::retryable(
            "MineSkin is rate-limiting uploads. Wait a minute and try again.",
            format!("429 from MineSkin: {body}"),
        ));
    }
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<ErrorBody>(body)
            .ok()
            .and_then(|e| e.errors.into_iter().next())
            .map(|e| e.message)
            .filter(|m| !m.is_empty());
        let message = match (status, detail) {
            (401 | 403, _) => "MineSkin rejected the launcher's API key.".to_string(),
            (400, Some(d)) => format!("MineSkin couldn't use that skin: {d}"),
            _ => "Couldn't upload that skin to MineSkin.".to_string(),
        };
        return Err(AppError::new(
            message,
            format!("{status} from MineSkin: {body}"),
        ));
    }
    let parsed: GenerateResponse = serde_json::from_str(body)?;
    let skin = parsed.skin.ok_or_else(|| {
        AppError::new(
            "MineSkin didn't return a skin. Try again.",
            format!("no `skin` in generate response: {body}"),
        )
    })?;
    Ok(MineSkinRef {
        uuid: skin.uuid,
        url: skin.texture.url.skin,
        variant: match skin.variant.as_str() {
            "classic" | "slim" => skin.variant,
            _ => requested_variant.to_string(),
        },
        uploaded_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{
        "success": true,
        "skin": {
            "uuid": "c891dfac4cd247a2a55743e7e82ce76f",
            "name": "Test Skin",
            "visibility": "unlisted",
            "variant": "slim",
            "views": 1,
            "duplicate": false,
            "texture": {
                "data": {"value": "v", "signature": "s"},
                "hash": {"skin": "abc"},
                "url": {"skin": "http://textures.minecraft.net/texture/abc"}
            },
            "generator": {}
        }
    }"#;

    #[test]
    fn parses_a_generated_skin() {
        let r = parse_generate(200, OK, "classic", 42).unwrap();
        assert_eq!(r.uuid, "c891dfac4cd247a2a55743e7e82ce76f");
        assert_eq!(r.url, "http://textures.minecraft.net/texture/abc");
        assert_eq!(r.variant, "slim");
        assert_eq!(r.uploaded_at, 42);
    }

    #[test]
    fn unknown_variant_falls_back_to_the_requested_one() {
        let body = OK.replace("\"slim\"", "\"unknown\"");
        assert_eq!(
            parse_generate(200, &body, "classic", 0).unwrap().variant,
            "classic"
        );
    }

    #[test]
    fn rate_limit_is_retryable() {
        let e = parse_generate(429, "{}", "classic", 0).unwrap_err();
        assert!(e.retryable);
    }

    #[test]
    fn surfaces_mineskin_validation_errors() {
        let body = r#"{"success":false,"errors":[{"code":"invalid_image","message":"Invalid image dimensions"}]}"#;
        let e = parse_generate(400, body, "classic", 0).unwrap_err();
        assert!(e.message.contains("Invalid image dimensions"));
    }

    #[test]
    fn missing_skin_is_an_error() {
        assert!(parse_generate(200, r#"{"success":true}"#, "classic", 0).is_err());
    }
}
