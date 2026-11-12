use std::sync::{Arc, Mutex};

use base64::Engine;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::{AppError, AppResult};
use crate::events::{Event, EventName, EventSink};
use crate::models::minecraft::{AuthSession, MinecraftProfile, SessionType};
use crate::services::cache::store::now_secs;
use crate::services::network::connectivity::prefer_ipv4;

const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const SCOPE: &str = "XboxLive.signin offline_access";

/// Microsoft's own special "native client" redirect URI — registering it
/// under a "Mobile and desktop applications" platform in Azure lets a
/// desktop app catch the OAuth redirect by watching for navigation to this
/// exact URL inside an embedded webview, with no local HTTP listener
/// needed. This is the same trick the official Minecraft Launcher, and
/// most native MSAL integrations, use for the interactive sign-in popup.
const NATIVE_REDIRECT_URI: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";
/// Label/title the desktop shell's embedded sign-in window uses, kept so
/// the OAuth flow's window contract stays one string defined here.
pub const LOGIN_WINDOW_LABEL: &str = "ms-login";

/// The host side of the sign-in window (per the MIGRATION.md §6 decision:
/// a small embedded browser window spawned only for OAuth). The crate that
/// owns the windowing system implements this; `interactive_authorize`
/// below drives it through exactly the same lifecycles as the old Tauri
/// `WebviewWindowBuilder` flow: a navigation callback for every URL the
/// window opens (return `false` to cancel navigating — Microsoft's
/// `nativeclient` redirect is only *observed*, never loaded), and a
/// close-notify so a window the user closes reads as "cancelled".
///
/// `open_sign_in_window` may close a stale window from a previous
/// cancelled/failed attempt itself — the old code did that too, because
/// the "ms-login" window label must be unique.
pub trait LoginHost: Send + Sync {
    fn open_sign_in_window(
        &self,
        url: String,
        on_navigation: Arc<dyn Fn(&str) -> bool + Send + Sync>,
        on_closed: Arc<dyn Fn() + Send + Sync>,
    ) -> AppResult<Box<dyn LoginWindow>>;
}

pub trait LoginWindow: Send + Sync {
    fn close(&self);
}

/// Arcane Launcher's own Azure app registration, baked in by `build.rs`
/// (from `MICROSOFT_CLIENT_ID` or the gitignored `microsoft-client-id.key`)
/// so it isn't in source control. Empty in a build that has neither, in
/// which case Microsoft sign-in is unavailable.
pub const DEFAULT_MICROSOFT_CLIENT_ID: &str = env!("ARCANE_MICROSOFT_CLIENT_ID");

const NOT_CONFIGURED: &str = "Microsoft sign-in isn't set up in this build of the launcher.";

#[derive(Debug, Clone, Serialize)]
pub struct AuthProgress {
    pub stage: String,
    pub message: String,
}

fn emit_progress(sink: &Arc<dyn EventSink>, stage: &str, message: &str) {
    sink.emit(
        Event::raw(
            EventName::AuthProgress,
            AuthProgress {
                stage: stage.to_string(),
                message: message.to_string(),
            },
        )
        .expect("AuthProgress serializes"),
    );
}

pub fn offline_uuid(username: &str) -> uuid::Uuid {
    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{username}").as_bytes());
    let mut bytes: [u8; 16] = hasher.finalize().into();
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

pub fn create_offline_session(username: String) -> AppResult<AuthSession> {
    let username = username.trim().to_string();
    if username.is_empty() || username.len() > 16 {
        return Err(AppError::new(
            "Enter a username between 1 and 16 characters.",
            "invalid offline username",
        ));
    }
    Ok(AuthSession {
        session_type: SessionType::Offline,
        profile: MinecraftProfile {
            id: offline_uuid(&username).simple().to_string(),
            name: username,
        },
        access_token: None,
        refresh_token: None,
        expires_at: None,
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct XblAuthResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<XuiEntry>,
}

#[derive(Deserialize)]
struct XuiEntry {
    uhs: String,
}

#[derive(Deserialize)]
struct McLoginResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct McProfileResponse {
    id: String,
    name: String,
}

/// Generates a PKCE code verifier: three concatenated UUIDv4s (96 hex
/// chars, backed by the OS CSPRNG via the `uuid` crate's `v4` feature) —
/// comfortably within RFC 7636's 43-128 char range, and hex digits are
/// already valid "unreserved" PKCE characters so no extra encoding is needed.
fn generate_pkce_verifier() -> String {
    let mut verifier = String::with_capacity(96);
    for _ in 0..3 {
        verifier.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    verifier
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

enum InteractiveAuthResult {
    Code(String),
    Denied(String),
    Cancelled,
}

/// Opens a real Microsoft sign-in window (email, password, MFA, "stay
/// signed in?" — whatever that account needs) and waits for it to redirect
/// to Microsoft's `nativeclient` URI, catching the authorization code by
/// watching the window's own navigation instead of running a local server.
/// Same flow as before (PKCE → nativeclient redirect harvest, close =
/// cancelled, 600 s timeout) — the window itself is provided by the
/// [`LoginHost`] implementation.
async fn interactive_authorize(
    host: &dyn LoginHost,
    client_id: &str,
    code_challenge: &str,
) -> AppResult<String> {
    let mut auth_url = url::Url::parse(AUTHORIZE_URL).expect("static URL is valid");
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("response_mode", "query")
        .append_pair("redirect_uri", NATIVE_REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("prompt", "select_account")
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");

    let (tx, rx) = tokio::sync::oneshot::channel::<InteractiveAuthResult>();
    let tx = Arc::new(Mutex::new(Some(tx)));

    let tx_nav = tx.clone();
    let tx_closed = tx.clone();
    let window = host
        .open_sign_in_window(
            auth_url.to_string(),
            Arc::new(move |url: &str| -> bool {
                if !url.starts_with(NATIVE_REDIRECT_URI) {
                    return true;
                }
                let mut code = None;
                let mut error = None;
                if let Ok(parsed) = url::Url::parse(url) {
                    for (key, value) in parsed.query_pairs() {
                        match key.as_ref() {
                            "code" => code = Some(value.into_owned()),
                            "error_description" => error = Some(value.into_owned()),
                            "error" if error.is_none() => error = Some(value.into_owned()),
                            _ => {}
                        }
                    }
                }
                if let Some(sender) = tx_nav.lock().unwrap().take() {
                    let result = match code {
                        Some(code) => InteractiveAuthResult::Code(code),
                        None => InteractiveAuthResult::Denied(
                            error.unwrap_or_else(|| "access_denied".to_string()),
                        ),
                    };
                    let _ = sender.send(result);
                }
                // Cancels the actual navigation — there's nothing real to load
                // at that URL, we only needed to see it go by.
                false
            }),
            Arc::new(move || {
                if let Some(sender) = tx_closed.lock().unwrap().take() {
                    let _ = sender.send(InteractiveAuthResult::Cancelled);
                }
            }),
        )
        .map_err(|e| AppError::new("Couldn't open the sign-in window.", e.to_string()))?;

    let result = tokio::time::timeout(std::time::Duration::from_secs(600), rx).await;

    window.close();

    match result {
        Err(_) => Err(AppError::new(
            "Sign-in timed out. Please try again.",
            "interactive login timed out",
        )),
        Ok(Err(_)) => Err(AppError::new(
            "Sign-in was cancelled.",
            "auth channel dropped",
        )),
        Ok(Ok(InteractiveAuthResult::Code(code))) => Ok(code),
        Ok(Ok(InteractiveAuthResult::Denied(reason))) => {
            Err(AppError::new("Microsoft sign-in failed.", reason))
        }
        Ok(Ok(InteractiveAuthResult::Cancelled)) => Err(AppError::new(
            "Sign-in was cancelled.",
            "user closed the sign-in window",
        )),
    }
}

async fn exchange_auth_code(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    code_verifier: &str,
) -> AppResult<(String, Option<String>, u64)> {
    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", NATIVE_REDIRECT_URI),
            ("code_verifier", code_verifier),
            ("scope", SCOPE),
        ])
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AppError::new(
            "Microsoft sign-in failed.",
            format!("token exchange failed: {status} {body}"),
        ));
    }
    let body: TokenResponse = res.json().await?;
    if let Some(err) = body.error {
        return Err(AppError::new("Microsoft sign-in failed.", err));
    }
    let access_token = body.access_token.ok_or_else(|| {
        AppError::new(
            "Microsoft sign-in failed.",
            "token response missing access_token",
        )
    })?;
    Ok((
        access_token,
        body.refresh_token,
        body.expires_in.unwrap_or(3600),
    ))
}

#[derive(Deserialize)]
struct XstsErrorResponse {
    #[serde(rename = "XErr")]
    x_err: Option<u64>,
}

/// Microsoft's XSTS service reports account-eligibility problems (not
/// "wrong password" — those already got rejected earlier, at the Microsoft
/// login page itself) as a numeric `XErr` code rather than readable text.
/// These specific codes are stable and well documented across the
/// open-source Minecraft launcher ecosystem (PrismLauncher, etc.).
fn xsts_error_message(x_err: u64) -> &'static str {
    match x_err {
        2148916233 => "This Microsoft account doesn't have an Xbox profile yet. Create one at xbox.com, then try again.",
        2148916235 => "Xbox Live isn't available for this account's country or region.",
        2148916236 | 2148916237 => "This account needs adult verification on xbox.com before it can sign in here.",
        2148916238 => "This is a child account — an adult needs to add it to a Microsoft Family group first.",
        _ => "Xbox Live rejected this sign-in.",
    }
}

async fn exchange_for_minecraft(
    http: &reqwest::Client,
    ms_access_token: &str,
) -> AppResult<(String, u64)> {
    let res = http
        .post(XBL_AUTH_URL)
        .json(&serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={ms_access_token}")
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        }))
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AppError::new(
            "Xbox Live sign-in failed.",
            format!("XBL auth {status}: {body}"),
        ));
    }
    let xbl: XblAuthResponse = res.json().await.map_err(|e| {
        AppError::new(
            "Xbox Live sign-in failed.",
            format!("decoding XBL response: {e}"),
        )
    })?;

    let uhs = xbl
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .ok_or_else(|| AppError::new("Xbox Live sign-in failed.", "missing uhs claim"))?;

    let res = http
        .post(XSTS_AUTH_URL)
        .json(&serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl.token]
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }))
        .send()
        .await?;
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        let body = res.text().await.unwrap_or_default();
        let x_err = serde_json::from_str::<XstsErrorResponse>(&body)
            .ok()
            .and_then(|e| e.x_err);
        let message = x_err
            .map(xsts_error_message)
            .unwrap_or("Xbox Live rejected this sign-in.");
        return Err(AppError::new(
            message,
            format!("XSTS 401 (XErr {x_err:?}): {body}"),
        ));
    }
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AppError::new(
            "Xbox Live sign-in failed.",
            format!("XSTS {status}: {body}"),
        ));
    }
    let xsts: XblAuthResponse = res.json().await.map_err(|e| {
        AppError::new(
            "Xbox Live sign-in failed.",
            format!("decoding XSTS response: {e}"),
        )
    })?;

    let res = http
        .post(MC_LOGIN_URL)
        .json(&serde_json::json!({
            "identityToken": format!("XBL3.0 x={uhs};{}", xsts.token)
        }))
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(AppError::new(
            "Minecraft sign-in failed.",
            format!("login_with_xbox {status}: {body}"),
        ));
    }
    let mc_login: McLoginResponse = res.json().await.map_err(|e| {
        AppError::new(
            "Minecraft sign-in failed.",
            format!("decoding minecraft login response: {e}"),
        )
    })?;

    Ok((mc_login.access_token, mc_login.expires_in))
}

async fn fetch_profile(
    http: &reqwest::Client,
    mc_access_token: &str,
) -> AppResult<MinecraftProfile> {
    let res = http
        .get(MC_PROFILE_URL)
        .bearer_auth(mc_access_token)
        .send()
        .await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::new(
            "This Microsoft account doesn't own Minecraft.",
            "404 from minecraft profile endpoint",
        ));
    }
    if !res.status().is_success() {
        return Err(AppError::new(
            "Couldn't load the Minecraft profile.",
            format!("{}", res.status()),
        ));
    }
    let profile: McProfileResponse = res.json().await?;
    Ok(MinecraftProfile {
        id: profile.id,
        name: profile.name,
    })
}

/// How close to expiry (in seconds) a Minecraft token has to be before it's
/// treated as already expired — covers clock skew and a request that's in
/// flight when the token lapses.
const REFRESH_MARGIN_SECS: u64 = 300;

/// Whether this session's Minecraft access token is missing, expired, or
/// about to be, so a Mojang API call would be rejected with a 401. Offline
/// sessions never have one and never need it.
pub fn needs_refresh(session: &AuthSession) -> bool {
    if session.session_type != SessionType::Microsoft {
        return false;
    }
    match (&session.access_token, session.expires_at) {
        (Some(_), Some(expires_at)) => expires_at <= now_secs() + REFRESH_MARGIN_SECS,
        _ => true,
    }
}

/// Silently mints a fresh Minecraft access token from the saved Microsoft
/// refresh token (the same Microsoft → Xbox Live → XSTS → Minecraft chain as
/// an interactive sign-in, minus the login window). Minecraft tokens only
/// last about a day, so without this every skin/cape call starts failing
/// with a 401 the day after signing in.
///
/// Microsoft rotates refresh tokens, so the returned session carries the new
/// one and must be persisted by the caller.
pub async fn refresh_session(client_id: &str, session: &AuthSession) -> AppResult<AuthSession> {
    let expired = || {
        AppError::new(
            "Your Microsoft sign-in has expired. Sign in again to continue.",
            "no usable refresh token on session",
        )
    };
    if client_id.trim().is_empty() {
        return Err(AppError::new(
            NOT_CONFIGURED,
            "microsoft client id not configured",
        ));
    }
    let refresh_token = session
        .refresh_token
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or_else(expired)?;

    let http = prefer_ipv4(reqwest::Client::builder())
        .build()
        .expect("http client");

    let res = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", SCOPE),
        ])
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        // A 4xx means Microsoft rejected the refresh token itself (revoked,
        // expired after ~90 days idle, password changed) — only signing in
        // again fixes that. Anything else is a transient server problem.
        if status.is_client_error() {
            return Err(AppError::new(
                "Your Microsoft sign-in has expired. Sign in again to continue.",
                format!("refresh token rejected: {status} {body}"),
            ));
        }
        return Err(AppError::retryable(
            "Couldn't refresh your Microsoft sign-in. Try again in a moment.",
            format!("token refresh failed: {status} {body}"),
        ));
    }
    let body: TokenResponse = res.json().await?;
    if let Some(err) = body.error {
        return Err(AppError::new("Microsoft sign-in failed.", err));
    }
    let ms_access_token = body.access_token.ok_or_else(|| {
        AppError::new(
            "Microsoft sign-in failed.",
            "refresh response missing access_token",
        )
    })?;

    let (mc_access_token, mc_expires_in) = exchange_for_minecraft(&http, &ms_access_token).await?;

    Ok(AuthSession {
        session_type: SessionType::Microsoft,
        profile: session.profile.clone(),
        access_token: Some(mc_access_token),
        // Fall back to the old one if Microsoft didn't rotate it.
        refresh_token: body.refresh_token.or_else(|| session.refresh_token.clone()),
        expires_at: Some(now_secs() + mc_expires_in),
    })
}

pub async fn login_with_microsoft(
    host: &dyn LoginHost,
    sink: &Arc<dyn EventSink>,
    client_id: &str,
) -> AppResult<AuthSession> {
    if client_id.trim().is_empty() {
        return Err(AppError::new(
            NOT_CONFIGURED,
            "microsoft client id not configured",
        ));
    }
    let http = prefer_ipv4(reqwest::Client::builder())
        .build()
        .expect("http client");

    emit_progress(sink, "opening", "Opening the Microsoft sign-in window...");
    let verifier = generate_pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let code = interactive_authorize(host, client_id, &challenge).await?;

    emit_progress(sink, "token", "Finishing sign-in...");
    let (ms_access_token, refresh_token, expires_in) =
        exchange_auth_code(&http, client_id, &code, &verifier).await?;

    emit_progress(sink, "xbox", "Signing in to Xbox Live...");
    let (mc_access_token, mc_expires_in) = exchange_for_minecraft(&http, &ms_access_token).await?;

    emit_progress(sink, "profile", "Loading your Minecraft profile...");
    let profile = fetch_profile(&http, &mc_access_token).await?;

    let _ = expires_in;
    Ok(AuthSession {
        session_type: SessionType::Microsoft,
        profile,
        access_token: Some(mc_access_token),
        refresh_token,
        expires_at: Some(now_secs() + mc_expires_in),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(kind: SessionType, token: Option<&str>, expires_at: Option<u64>) -> AuthSession {
        AuthSession {
            session_type: kind,
            profile: MinecraftProfile {
                id: "id".into(),
                name: "name".into(),
            },
            access_token: token.map(String::from),
            refresh_token: Some("r".into()),
            expires_at,
        }
    }

    #[test]
    fn offline_sessions_never_refresh() {
        assert!(!needs_refresh(&session(SessionType::Offline, None, None)));
    }

    #[test]
    fn valid_token_is_kept() {
        let s = session(SessionType::Microsoft, Some("t"), Some(now_secs() + 3600));
        assert!(!needs_refresh(&s));
    }

    #[test]
    fn expired_or_nearly_expired_token_refreshes() {
        assert!(needs_refresh(&session(
            SessionType::Microsoft,
            Some("t"),
            Some(now_secs() - 10)
        )));
        assert!(needs_refresh(&session(
            SessionType::Microsoft,
            Some("t"),
            Some(now_secs() + 60)
        )));
    }

    #[test]
    fn missing_token_or_expiry_refreshes() {
        assert!(needs_refresh(&session(
            SessionType::Microsoft,
            None,
            Some(now_secs() + 3600)
        )));
        assert!(needs_refresh(&session(
            SessionType::Microsoft,
            Some("t"),
            None
        )));
    }
}
