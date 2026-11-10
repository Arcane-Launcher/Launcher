use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::error::AppError;
use crate::models::common::ProjectType;
use crate::paths::safe_join;
use crate::services::instance::crash_help::{self, CrashAnalysis, FixAction};
use crate::services::instance::install::{install_mod, InstallModRequest};
use crate::services::instance::mods as instance_mods;
use crate::services::minecraft::java_provision;
use crate::state::AppState;

/// Reads the instance's last session and explains what went wrong.
pub async fn analyze_crash(state: &AppState, instance_id: String) -> Result<CrashAnalysis, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    Ok(crash_help::analyze(&instance, &state.running, &state.providers).await)
}

/// Applies one of the crash helper's fixes. Returns what was done, in a
/// sentence for the toast.
pub async fn apply_crash_fix(
    state: &AppState,
    instance_id: String,
    action: FixAction,
) -> Result<String, AppError> {
    if state.running.get(&instance_id).await.is_some() {
        return Err(AppError::new("Close the game first.", "apply_crash_fix: instance is running"));
    }
    let mut instance = state.instances.get(&instance_id).await?;

    match action {
        FixAction::SetMemory { mb } => {
            let mb = mb.clamp(512, 65536);
            instance.memory_mb = Some(mb);
            state.instances.update(instance).await?;
            Ok(format!("Memory set to {mb} MB."))
        }
        FixAction::UseJava { major } => {
            let path = java_provision::ensure_java(major, &state.paths, &state.downloads).await?;
            instance.java_path = Some(path);
            state.instances.update(instance).await?;
            Ok(format!("This instance now uses Java {major}."))
        }
        FixAction::Repair => {
            super::launch::fix_installation(&state, &mut instance).await;
            Ok("Game files repaired.".into())
        }
        FixAction::InstallMod { source, project_id, name } => {
            let req = InstallModRequest {
                instance_id: instance.id.clone(),
                project_id,
                version_id: None,
                source,
                project_type: ProjectType::Mod,
                world: None,
            };
            let outcome = install_mod(&state.instances, &state.providers, &state.downloads, req, true)
                .await
                .map_err(|e| {
                    if e.message == "Already installed." {
                        AppError::new(
                            format!("{name} is already on its newest version for this Minecraft version."),
                            e.technical,
                        )
                    } else {
                        e
                    }
                })?;
            let deps: Vec<String> = outcome.installed_dependencies.iter().map(|d| d.name.clone()).collect();
            Ok(if deps.is_empty() {
                format!("Installed {}.", outcome.installed.name)
            } else {
                format!("Installed {} (plus {}).", outcome.installed.name, deps.join(", "))
            })
        }
        FixAction::DisableMod { file_name, name } => {
            let installed = instance_mods::list_installed(&instance).await?;
            if let Some(tracked) = installed
                .iter()
                .find(|m| m.enabled && m.project_type == ProjectType::Mod && m.file_name == file_name)
            {
                instance_mods::set_enabled(&instance, &tracked.id, false).await?;
                return Ok(format!("Turned off {name}. You can turn it back on in the Mods tab."));
            }
            // A jar the launcher didn't install: park it in the same
            // `.disabled` folder tracked mods use, so nothing is deleted.
            let mods_dir = instance_mods::mods_dir(&instance);
            let from = safe_join(&mods_dir, &file_name)
                .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
            let disabled_dir = instance_mods::disabled_dir(&instance, ProjectType::Mod, None);
            let to = safe_join(&disabled_dir, &file_name)
                .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
            if !from.is_file() {
                return Err(AppError::new(format!("{file_name} isn't in the mods folder anymore."), "disable: file missing"));
            }
            tokio::fs::create_dir_all(&disabled_dir).await?;
            tokio::fs::rename(&from, &to).await?;
            Ok(format!("Turned off {name} (moved to mods\\.disabled)."))
        }
        FixAction::OpenUrl { .. } => Err(AppError::new("Open links from the launcher window.", "OpenUrl is handled by the UI")),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedLog {
    pub log_url: Option<String>,
    pub crash_report_url: Option<String>,
}

/// mclo.gs caps a paste at 25,000 lines / 10 MiB.
const MAX_SHARE_LINES: usize = 20_000;
const MAX_SHARE_BYTES: usize = 9 * 1024 * 1024;

/// Uploads the instance's last session log (and crash report, if one was
/// written) to mclo.gs and returns the links. The player's account tokens
/// and Windows user folder are removed first; mclo.gs itself also strips IP
/// addresses.
pub async fn share_instance_log(state: &AppState, instance_id: String) -> Result<SharedLog, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let end = state.running.last_exit(&instance_id).await;
    let text = crash_help::gather(&instance, &state.running, end.as_ref()).await;
    if text.log.trim().is_empty() && text.report.is_none() {
        return Err(AppError::new(
            "There's no log to share yet. Launch the game first.",
            "share_instance_log: no log or crash report",
        ));
    }

    let mut secrets: Vec<String> = Vec::new();
    for account in state.session.list().await {
        secrets.extend(account.access_token.into_iter().chain(account.refresh_token).filter(|t| t.len() >= 8));
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok();
    let redact = |text: &str| redact(text, &secrets, home.as_deref());

    let client = reqwest::Client::builder()
        .user_agent(concat!("ArcaneLauncher/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::new("Couldn't upload the log.", e.to_string()))?;

    // Shown next to the log on mclo.gs, so whoever helps can see the setup
    // at a glance.
    let loader = match &instance.loader_version {
        Some(v) => format!("{} {v}", instance.loader.id()),
        None => instance.loader.id().to_string(),
    };
    let metadata = serde_json::json!([
        { "key": "launcher", "label": "Launcher", "value": concat!("Arcane Launcher ", env!("CARGO_PKG_VERSION")) },
        { "key": "minecraft", "label": "Minecraft", "value": instance.minecraft_version },
        { "key": "loader", "label": "Loader", "value": loader },
        { "key": "memory", "label": "Memory", "value": instance.memory_mb.map(|m| format!("{m} MB")).unwrap_or_else(|| "Java default".into()) },
    ]);

    let log_url = if text.log.trim().is_empty() {
        None
    } else {
        Some(upload(&client, &cap(&redact(&text.log)), &metadata).await?)
    };
    let crash_report_url = match &text.report {
        Some((_, report)) => Some(upload(&client, &cap(&redact(report)), &metadata).await?),
        None => None,
    };
    Ok(SharedLog { log_url, crash_report_url })
}

fn redact(text: &str, secrets: &[String], home: Option<&str>) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        out = out.replace(secret.as_str(), "[token removed]");
    }
    if let Some(home) = home.filter(|h| h.len() > 3) {
        // Both slash styles: Java prints either depending on the source.
        for form in [home.to_string(), home.replace('\\', "/")] {
            out = out.replace(&form, "~");
        }
        if let Some(user) = Path::new(home).file_name().and_then(|u| u.to_str()) {
            out = out.replace(&format!("\\Users\\{user}\\"), "\\Users\\~\\");
        }
    }
    out
}

/// Keeps the end of an oversized log — the crash is at the bottom.
fn cap(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(MAX_SHARE_LINES);
    let mut kept = lines[start..].join("\n");
    if kept.len() > MAX_SHARE_BYTES {
        let mut cut = kept.len() - MAX_SHARE_BYTES;
        while !kept.is_char_boundary(cut) {
            cut += 1;
        }
        kept = kept[cut..].to_string();
    }
    kept
}

async fn upload(client: &reqwest::Client, content: &str, metadata: &serde_json::Value) -> Result<String, AppError> {
    #[derive(serde::Deserialize)]
    struct Response {
        success: bool,
        url: Option<String>,
        error: Option<String>,
    }
    let failed = |technical: String| AppError::new("Couldn't upload the log. Check your internet connection.", technical);
    let res = client
        .post("https://api.mclo.gs/1/log")
        .json(&serde_json::json!({ "content": content, "source": "Arcane Launcher", "metadata": metadata }))
        .send()
        .await
        .map_err(|e| failed(e.to_string()))?;
    let status = res.status();
    let body: Response = res.json().await.map_err(|e| failed(format!("HTTP {status}: {e}")))?;
    match (body.success, body.url) {
        (true, Some(url)) => Ok(url),
        _ => Err(failed(format!("HTTP {status}: {}", body.error.unwrap_or_default()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_and_home_folder() {
        let text = "--accessToken eyJhbGciOiJIUzI1NiJ9.secret\nLoading C:\\Users\\Maneka\\AppData\\x.jar and C:/Users/Maneka/y";
        let out = redact(text, &["eyJhbGciOiJIUzI1NiJ9.secret".into()], Some("C:\\Users\\Maneka"));
        assert_eq!(out, "--accessToken [token removed]\nLoading ~\\AppData\\x.jar and ~/y");
    }

    #[test]
    fn caps_to_the_last_lines() {
        let text = (0..MAX_SHARE_LINES + 5).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let capped = cap(&text);
        assert!(capped.starts_with("5\n"));
        assert_eq!(capped.lines().count(), MAX_SHARE_LINES);
    }
}
