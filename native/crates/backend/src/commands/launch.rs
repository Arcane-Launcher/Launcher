use crate::error::AppError;
use crate::events::{Event, EventName};
use crate::models::common::{DependencyType, ModLoaderKind, ProjectType};
use crate::models::instance::MinecraftInstance;
use crate::services::instance::health;
use crate::services::instance::install::{install_mod, InstallModRequest};
use crate::services::instance::mods as instance_mods;
use crate::services::instance::update_check;
use crate::services::instance::validation;
use crate::services::minecraft::java::{self, DetectedJava};
use crate::services::minecraft::java_provision;
use crate::services::minecraft::running::{LogLine, ProcessStats, RunningInfo};
use crate::state::AppState;

/// Resolves the Java to launch with: an instance's own Java path is the
/// player's explicit choice and is honored whenever it starts (Smart Launch
/// warns if it's the wrong version). The global default is only a fallback
/// — often just the first Java found on the system — so it's used only when
/// it's exactly the major version this Minecraft version needs; otherwise a
/// matching Eclipse Temurin JRE is downloaded, and the player never has to
/// go find and install Java by hand.
async fn resolve_or_provision_java(
    state: &AppState,
    instance: &MinecraftInstance,
    required_major: u32,
) -> Result<String, AppError> {
    if let Some(path) = instance.java_path.clone() {
        if java::is_valid_java(&path).await {
            return Ok(path);
        }
    }
    if let Some(path) = state.settings.get().await.default_java_path {
        if java::major_version_of(&path).await == Some(required_major) {
            return Ok(path);
        }
    }
    java_provision::ensure_java(required_major, &state.paths, &state.downloads).await
}

pub(crate) async fn ensure_loader_ready(
    state: &AppState,
    instance: &mut MinecraftInstance,
) -> Result<String, AppError> {
    crate::services::loaders::ensure_loader_ready(
        &state.loaders,
        &state.engine,
        &state.instances,
        instance,
    )
    .await
}

#[derive(Clone, serde::Serialize)]
struct InstallStage {
    instance_id: String,
    stage: String,
}

/// Tells the UI what an install is doing during the steps that aren't file
/// downloads (fetching version info from Mojang, installing a mod loader)
/// — those can take a while on a slow connection and used to show nothing
/// but "Preparing...".
fn emit_stage(state: &AppState, instance: &MinecraftInstance, stage: impl Into<String>) {
    state.events.emit(
        Event::raw(
            EventName::InstallStage,
            InstallStage {
                instance_id: instance.id.clone(),
                stage: stage.into(),
            },
        )
        .expect("InstallStage serializes"),
    );
}

/// The shared "get everything this instance needs onto disk" sequence,
/// with a progress stage announced before each step.
async fn install_with_stages(
    state: &AppState,
    instance: &mut MinecraftInstance,
) -> Result<String, AppError> {
    if instance.loader != ModLoaderKind::Vanilla {
        let loader = match instance.loader {
            ModLoaderKind::Fabric => "Fabric",
            ModLoaderKind::Quilt => "Quilt",
            ModLoaderKind::Forge => "Forge",
            ModLoaderKind::NeoForge => "NeoForge",
            ModLoaderKind::Vanilla => "",
        };
        emit_stage(state, instance, format!("Setting up {loader}..."));
    }
    let version_key = ensure_loader_ready(state, instance).await?;
    emit_stage(
        state,
        instance,
        format!("Getting Minecraft {} info...", instance.minecraft_version),
    );
    state
        .engine
        .manifest
        .ensure_vanilla_version_detail(&instance.minecraft_version)
        .await?;
    emit_stage(state, instance, "Starting downloads...");
    Ok(version_key)
}

async fn defer_optional_assets(state: &AppState) -> bool {
    !state.settings.get().await.full_assets_before_launch
}

pub async fn ensure_instance_installed(
    state: &AppState,
    instance_id: String,
) -> Result<(), AppError> {
    let mut instance = state.instances.get(&instance_id).await?;
    let version_key = install_with_stages(state, &mut instance).await?;
    let defer = defer_optional_assets(state).await;
    state
        .engine
        .ensure_installed(&instance, &version_key, defer)
        .await?;
    Ok(())
}

pub async fn launch_instance(
    state: &AppState,
    instance_id: String,
    target_server: Option<String>,
) -> Result<(), AppError> {
    let mut instance = state.instances.get(&instance_id).await?;

    // Refreshes an expired Microsoft token first — the stored one only lasts
    // about a day, and online servers reject a stale one. A failed refresh
    // (usually no internet) still launches with the saved session, since
    // singleplayer doesn't need a valid token.
    let session = match super::auth::fresh_session(state).await {
        Ok(session) => session,
        Err(err) => {
            log::warn!(
                "launch: couldn't refresh the Minecraft token, using the saved one: {err:?}"
            );
            state.session.get().await
        }
    };
    let session = session.ok_or_else(|| {
        AppError::new(
            "Sign in (or play offline) before launching.",
            "no active session",
        )
    })?;

    let version_key = install_with_stages(state, &mut instance).await?;
    let defer = defer_optional_assets(state).await;
    let required_major = state
        .engine
        .manifest
        .resolve_effective_version(&version_key)
        .await?
        .java_version
        .map(|j| j.major_version)
        .unwrap_or(java_provision::DEFAULT_JAVA_MAJOR);

    // A first launch often needs a Java runtime (~45 MB) as well as the
    // game files; fetch both at once rather than one after the other.
    emit_stage(state, &instance, "Checking game files and Java...");
    let (version, java_path) = tokio::try_join!(
        state
            .engine
            .ensure_installed(&instance, &version_key, defer),
        resolve_or_provision_java(state, &instance, required_major),
    )?;

    // Which graphics card the game runs on (laptops with two GPUs).
    let gpu_preference = state.settings.get().await.gpu_preference;
    crate::services::gpu::apply_for_launch(state.paths.root(), gpu_preference, &java_path);
    emit_stage(state, &instance, "Starting Minecraft...");

    state
        .engine
        .launch(
            state,
            &instance,
            &version,
            &session,
            &java_path,
            target_server.as_deref(),
            crate::services::gpu::launch_env(gpu_preference),
            state.running.clone(),
            state.instances.clone(),
        )
        .await?;
    state.instances.touch_last_played(&instance_id).await?;

    // Live presence from here on follows the game's own log (main menu,
    // singleplayer, which server) — see `discord_rpc::parse_game_log`.
    // Discord's small image is the player's head: by UUID for Microsoft
    // accounts, by the uploaded skin's texture for offline ones.
    let head_skin_id = match session.session_type {
        crate::models::minecraft::SessionType::Microsoft => session.profile.id.clone(),
        crate::models::minecraft::SessionType::Offline => {
            crate::services::minecraft::skins::active_texture_hash(
                &state.paths,
                &session.profile.id,
            )
            .await
            .unwrap_or_else(|| crate::services::discord_rpc::DEFAULT_HEAD.to_string())
        }
    };
    state
        .discord
        .start_game(
            &state.settings,
            &instance,
            target_server.as_deref(),
            &session.profile.name,
            &head_skin_id,
        )
        .await;

    Ok(())
}

/// A join link that started the launcher, collected once by the UI when it
/// mounts (see `services::join_link`).
pub async fn take_pending_join(
    state: &AppState,
) -> Result<Option<crate::services::discord_rpc::DiscordJoinEvent>, AppError> {
    Ok(state
        .pending_join
        .0
        .lock()
        .map(|mut p| p.take())
        .unwrap_or(None))
}

/// Answers whichever "Ask to Join" Discord request is currently pending —
/// see [`crate::services::discord_rpc::DiscordPresence::respond_join_request`].
pub async fn respond_discord_join_request(state: &AppState, accept: bool) -> Result<(), AppError> {
    state.discord.respond_join_request(accept);
    Ok(())
}

/// The launcher page the UI is showing ("Browsing mods", ...), shown as the
/// Discord status while no game is running. Capped in length since it's
/// displayed verbatim.
pub async fn set_discord_page(state: &AppState, page: Option<String>) -> Result<(), AppError> {
    let page = page
        .map(|p| p.chars().take(64).collect::<String>())
        .filter(|p| !p.trim().is_empty());
    state.discord.set_page(&state.settings, page);
    Ok(())
}

/// Redownloads whatever the loader/engine finds missing, and drops any mod
/// metadata entries whose file is missing/corrupted or duplicated. Never
/// deletes a mod file that is actually present and valid. Shared by the
/// launch-validation screen's "Fix" and Smart Launch's "Fix All".
pub(crate) async fn fix_installation(state: &AppState, instance: &mut MinecraftInstance) {
    let _ = instance_mods::cleanup_broken_entries(instance).await;
    if let Ok(version_key) = ensure_loader_ready(state, instance).await {
        let _ = state
            .engine
            .ensure_installed(instance, &version_key, false)
            .await;
    }
}

pub async fn fix_instance(
    state: &AppState,
    instance_id: String,
) -> Result<crate::models::instance::InstanceValidationReport, AppError> {
    let mut instance = state.instances.get(&instance_id).await?;
    fix_installation(state, &mut instance).await;
    let default_java = state.settings.get().await.default_java_path;
    Ok(validation::validate_instance(
        &instance,
        &state.paths,
        &state.engine.manifest,
        default_java,
    )
    .await)
}

/// Runs every Arcane Smart Launch pre-flight check for this instance.
pub async fn check_instance_health(
    state: &AppState,
    instance_id: String,
) -> Result<health::HealthReport, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let default_java = state.settings.get().await.default_java_path;
    Ok(health::check(
        &instance,
        &state.paths,
        &state.engine.manifest,
        &state.providers,
        default_java,
    )
    .await)
}

/// Applies every automatic remedy Smart Launch found (see each check's own
/// `fixable` flag) — installation files, Java version mismatch, outdated
/// mods, and missing required dependencies. Mod conflicts, low disk space,
/// and memory sizing are never touched here since they need the player's
/// own judgment, not a guess on Arcane's part.
pub async fn fix_instance_health(
    state: &AppState,
    instance_id: String,
) -> Result<health::HealthReport, AppError> {
    let mut instance = state.instances.get(&instance_id).await?;
    let default_java = state.settings.get().await.default_java_path;
    let report = health::check(
        &instance,
        &state.paths,
        &state.engine.manifest,
        &state.providers,
        default_java,
    )
    .await;
    let needs_fix = |id: &str| {
        report
            .items
            .iter()
            .any(|i| i.id == id && i.fixable && i.status != health::HealthStatus::Ok)
    };

    if needs_fix("installation") {
        fix_installation(state, &mut instance).await;
    }

    if needs_fix("java") {
        if let Some(key) = validation::expected_version_key(&instance) {
            if let Some(required) = state
                .engine
                .manifest
                .resolve_effective_version_offline(&key)
                .await
                .and_then(|v| v.java_version)
            {
                if let Ok(path) = java_provision::ensure_java(
                    required.major_version,
                    &state.paths,
                    &state.downloads,
                )
                .await
                {
                    instance.java_path = Some(path);
                }
            }
        }
    }

    let auto_deps = state
        .settings
        .get()
        .await
        .auto_install_required_dependencies;

    if needs_fix("outdated_mods") {
        let installed = instance_mods::list_installed(&instance)
            .await
            .unwrap_or_default();
        update_check::apply_all_updates(
            &instance,
            &installed,
            &state.instances,
            &state.providers,
            &state.downloads,
            auto_deps,
        )
        .await;
    }

    if needs_fix("dependencies") {
        let installed = instance_mods::list_installed(&instance)
            .await
            .unwrap_or_default();
        let installed_ids: std::collections::HashSet<String> =
            installed.iter().map(|m| m.project_id.clone()).collect();
        for m in &installed {
            for dep in &m.dependencies {
                if dep.dependency_type != DependencyType::Required {
                    continue;
                }
                let Some(dep_id) = &dep.project_id else {
                    continue;
                };
                if installed_ids.contains(dep_id) {
                    continue;
                }
                let req = InstallModRequest {
                    instance_id: instance.id.clone(),
                    project_id: dep_id.clone(),
                    version_id: dep.version_id.clone(),
                    source: m.source,
                    project_type: ProjectType::Mod,
                    world: None,
                };
                let _ = install_mod(
                    &state.instances,
                    &state.providers,
                    &state.downloads,
                    req,
                    auto_deps,
                )
                .await;
            }
        }
    }

    state.instances.update(instance.clone()).await?;

    let default_java = state.settings.get().await.default_java_path;
    Ok(health::check(
        &instance,
        &state.paths,
        &state.engine.manifest,
        &state.providers,
        default_java,
    )
    .await)
}

pub async fn detect_java() -> Result<Vec<DetectedJava>, AppError> {
    Ok(java::detect_java_installations().await)
}

/// The Settings page's silent Java check: if a valid default is already
/// configured, confirm it and return early. Otherwise look for any Java
/// already on the system; failing that, download a matching runtime — the
/// user never has to notice Java was missing at all.
pub async fn ensure_default_java(state: &AppState) -> Result<DetectedJava, AppError> {
    if let Some(path) = state.settings.get().await.default_java_path {
        if java::is_valid_java(&path).await {
            let version = java::probe_version(std::path::Path::new(&path))
                .await
                .unwrap_or_default();
            return Ok(DetectedJava { path, version });
        }
    }

    if let Some(found) = java::detect_java_installations().await.into_iter().next() {
        state
            .settings
            .update(|s| s.default_java_path = Some(found.path.clone()))
            .await?;
        return Ok(found);
    }

    let path = java_provision::ensure_java(
        java_provision::DEFAULT_JAVA_MAJOR,
        &state.paths,
        &state.downloads,
    )
    .await?;
    let version = java::probe_version(std::path::Path::new(&path))
        .await
        .unwrap_or_default();
    state
        .settings
        .update(|s| s.default_java_path = Some(path.clone()))
        .await?;
    Ok(DetectedJava { path, version })
}

pub async fn validate_java_path(path: String) -> Result<bool, AppError> {
    Ok(java::is_valid_java(&path).await)
}

pub async fn list_running_instances(state: &AppState) -> Result<Vec<RunningInfo>, AppError> {
    Ok(state.running.list().await)
}

/// The buffered console output (stdout+stderr, interleaved in arrival
/// order) from this instance's most recent session — used both for a live
/// console while it's running and to inspect what happened after a crash.
pub async fn get_instance_log(
    state: &AppState,
    instance_id: String,
) -> Result<Vec<LogLine>, AppError> {
    Ok(state.running.get_log(&instance_id).await)
}

/// Live CPU/RAM usage for a running instance's game process, polled from
/// the frontend while a console panel showing it is open — `None` if the
/// instance isn't running.
pub async fn get_process_stats(
    state: &AppState,
    instance_id: String,
) -> Result<Option<ProcessStats>, AppError> {
    Ok(state.running.process_stats(&instance_id).await)
}

/// Force-quits a running instance's game process. This is a blunt
/// instrument (no save-and-exit, no warning to the player in-game) — it
/// exists for when the normal in-game quit isn't an option.
pub async fn kill_instance(state: &AppState, instance_id: String) -> Result<(), AppError> {
    let Some(info) = state.running.get(&instance_id).await else {
        return Err(AppError::new(
            "This instance isn't running.",
            "kill_instance: not tracked as running",
        ));
    };
    let Some(pid) = info.pid else {
        return Err(AppError::new(
            "Couldn't determine this instance's process.",
            "kill_instance: no pid recorded",
        ));
    };
    state.running.mark_killed(&instance_id).await;
    if crate::services::minecraft::running::kill_pid(pid) {
        Ok(())
    } else {
        Err(AppError::new(
            "Couldn't stop this instance's process. It may have already exited.",
            format!("kill_pid({pid}) reported failure"),
        ))
    }
}
