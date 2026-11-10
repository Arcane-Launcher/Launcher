use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::{AppError, AppResult};
use crate::models::instance::MinecraftInstance;
use crate::models::minecraft::{ArgumentValue, ArgumentValueInner, AuthSession, SessionType, VersionDetail};
use crate::paths::AppPaths;
use crate::services::cache::store::now_secs;
use crate::services::instance::manager::InstanceManager;
use crate::events::{Event, EventName};

use super::libraries::{resolve_library_artifact, rules_allow};
use super::running::RunningInstances;

#[derive(Debug, Clone, Serialize)]
pub struct LaunchLogEvent {
    pub instance_id: String,
    pub line: String,
    pub stream: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchStartedEvent {
    pub instance_id: String,
    pub started_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LaunchExitEvent {
    pub instance_id: String,
    pub code: Option<i32>,
    /// See [`crate::services::minecraft::running::SessionEnd::crashed`].
    pub crashed: bool,
}

fn classpath_separator() -> &'static str {
    if cfg!(target_os = "windows") {
        ";"
    } else {
        ":"
    }
}

fn substitute(template: &str, vars: &HashMap<&str, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("${{{key}}}"), value);
    }
    result
}

fn expand_argument_list(args: &[ArgumentValue], vars: &HashMap<&str, String>) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        match arg {
            ArgumentValue::Simple(s) => out.push(substitute(s, vars)),
            ArgumentValue::Conditional { rules, value } => {
                if rules_allow(rules) {
                    match value {
                        ArgumentValueInner::Single(s) => out.push(substitute(s, vars)),
                        ArgumentValueInner::Multiple(list) => {
                            out.extend(list.iter().map(|s| substitute(s, vars)))
                        }
                    }
                }
            }
        }
    }
    out
}

#[derive(Debug)]
pub struct LaunchPlan {
    pub java_path: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    /// Extra environment for the game process (e.g. GPU selection).
    pub env: Vec<(String, String)>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_launch_plan(
    instance: &MinecraftInstance,
    version: &VersionDetail,
    session: &AuthSession,
    java_path: &str,
    natives_dir: &Path,
    paths: &AppPaths,
    target_server: Option<&str>,
) -> AppResult<LaunchPlan> {
    // Keyed by the vanilla Minecraft version, not `version.id` (which for a
    // Fabric/Quilt/Forge profile is the loader's merged profile id) since
    // the client jar itself is identical across every loader on that
    // version and should only ever be downloaded once.
    let client_jar = paths
        .versions_meta_dir()
        .join(crate::paths::sanitize_component(&instance.minecraft_version))
        .join("client.jar");
    if !client_jar.exists() {
        return Err(AppError::new(
            "The Minecraft client is missing. Repair this instance before launching.",
            format!("client.jar missing at {}", client_jar.display()),
        ));
    }

    let mut classpath: Vec<String> = version
        .libraries
        .iter()
        .filter(|lib| crate::services::minecraft::libraries::library_allowed(&lib.rules))
        .filter_map(resolve_library_artifact)
        .map(|resolved| {
            paths
                .libraries_dir()
                .join(resolved.relative_path)
                .to_string_lossy()
                .to_string()
        })
        .collect();
    classpath.push(client_jar.to_string_lossy().to_string());
    let classpath_str = classpath.join(classpath_separator());

    let asset_index_name = version
        .asset_index
        .as_ref()
        .map(|a| a.id.clone())
        .or_else(|| version.assets.clone())
        .unwrap_or_else(|| "legacy".to_string());

    let (auth_player_name, auth_uuid, auth_access_token, user_type) = match session.session_type {
        SessionType::Microsoft => (
            session.profile.name.clone(),
            session.profile.id.clone(),
            session.access_token.clone().unwrap_or_default(),
            "msa".to_string(),
        ),
        SessionType::Offline => (
            session.profile.name.clone(),
            session.profile.id.clone(),
            "0".to_string(),
            "legacy".to_string(),
        ),
    };

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("natives_directory", natives_dir.to_string_lossy().to_string());
    vars.insert("launcher_name", "ArcaneLauncher".to_string());
    vars.insert("launcher_version", "0.1.0".to_string());
    vars.insert("classpath", classpath_str.clone());
    vars.insert("auth_player_name", auth_player_name);
    vars.insert("version_name", version.id.clone());
    vars.insert("game_directory", instance.game_dir.clone());
    vars.insert(
        "assets_root",
        paths.assets_dir().to_string_lossy().to_string(),
    );
    vars.insert("assets_index_name", asset_index_name);
    vars.insert("auth_uuid", auth_uuid);
    vars.insert("auth_access_token", auth_access_token);
    vars.insert("clientid", "arcane-launcher".to_string());
    vars.insert("auth_xuid", "0".to_string());
    vars.insert("user_type", user_type);
    vars.insert("version_type", "Arcane Launcher".to_string());

    let mut args: Vec<String> = Vec::new();

    if let Some(arguments) = &version.arguments {
        args.extend(expand_argument_list(&arguments.jvm, &vars));
    } else {
        args.push(format!("-Djava.library.path={}", natives_dir.to_string_lossy()));
        args.push("-cp".to_string());
        args.push(classpath_str.clone());
    }

    if let Some(memory_mb) = instance.memory_mb {
        args.push(format!("-Xmx{memory_mb}M"));
        args.push(format!("-Xms{}M", (memory_mb / 2).max(512)));
    }

    if let Some(java_args) = &instance.java_args {
        args.extend(java_args.split_whitespace().map(|s| s.to_string()));
    }

    args.push(version.main_class.clone());

    // `minecraft_arguments` wins when present: legacy (pre-1.13) profiles
    // only ever have this field, but Forge/NeoForge's synthetic ForgeWrapper
    // profile deliberately carries both — a legacy-style game argument
    // string (what ForgeWrapper expects) alongside a structured
    // `arguments.jvm` (for its `-Dforgewrapper.*` flags) — so checking
    // `arguments` first would wrongly pick up its empty `game` list.
    if let Some(legacy) = &version.minecraft_arguments {
        args.extend(legacy.split_whitespace().map(|s| substitute(s, &vars)));
    } else if let Some(arguments) = &version.arguments {
        args.extend(expand_argument_list(&arguments.game, &vars));
    }

    if let (Some(width), Some(height)) = (instance.window_width, instance.window_height) {
        args.push("--width".to_string());
        args.push(width.to_string());
        args.push("--height".to_string());
        args.push(height.to_string());
    }

    // Quick Play (1.20+) auto-connects to a server on launch instead of
    // stopping at the title screen. Older clients simply ignore the
    // unrecognized flag and boot normally, so this is safe to always pass.
    if let Some(address) = target_server {
        args.push("--quickPlayMultiplayer".to_string());
        args.push(address.to_string());
    }

    Ok(LaunchPlan {
        java_path: java_path.to_string(),
        args,
        working_dir: PathBuf::from(&instance.game_dir),
        env: Vec::new(),
    })
}

/// Runs a user-configured pre-launch/post-exit shell command through the OS
/// shell, with the same `INST_*` environment variables PrismLauncher exposes
/// to its own custom commands — so anyone bringing scripts over from there
/// doesn't need to rewrite them. Best-effort: a failing or missing command
/// is logged but never blocks the actual launch.
async fn run_hook_command(instance: &MinecraftInstance, java_path: &str, java_args: &[String], command: &str) {
    if command.trim().is_empty() {
        return;
    }
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(&instance.game_dir)
        .env("INST_NAME", &instance.name)
        .env("INST_ID", &instance.id)
        .env("INST_DIR", &instance.game_dir)
        .env("INST_MC_DIR", &instance.game_dir)
        .env("INST_JAVA", java_path)
        .env("INST_JAVA_ARGS", java_args.join(" "));
    crate::services::process_ext::suppress_console_window(&mut cmd);

    match cmd.status().await {
        Ok(status) if !status.success() => {
            log::error!("hook command exited with {status}: {command}");
        }
        Err(e) => log::error!("failed to run hook command '{command}': {e}"),
        _ => {}
    }
}

pub async fn spawn_and_stream(
    state: &crate::state::AppState,
    instance: &MinecraftInstance,
    plan: LaunchPlan,
    running: RunningInstances,
    instances: InstanceManager,
) -> AppResult<()> {
    let instance_id = instance.id.as_str();
    std::fs::create_dir_all(&plan.working_dir)?;

    if let Some(command) = &instance.pre_launch_command {
        run_hook_command(instance, &plan.java_path, &plan.args, command).await;
    }

    let mut launch_cmd = tokio::process::Command::new(&plan.java_path);
    launch_cmd
        .args(&plan.args)
        .envs(plan.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&plan.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::services::process_ext::suppress_console_window(&mut launch_cmd);
    let mut child = launch_cmd
        .spawn()
        .map_err(|e| {
            AppError::new(
                "Minecraft couldn't be started. Check that Java is configured correctly.",
                format!("failed to spawn java: {e}"),
            )
        })?;

    let started_at = now_secs();
    let pid = child.id();
    running.mark_started(instance_id, started_at, pid).await;
    let _ = state.events.emit(
        Event::raw(
            EventName::LaunchStarted,
            LaunchStartedEvent {
                instance_id: instance_id.to_string(),
                started_at,
            },
        )
        .expect("LaunchStartedEvent serializes"),
    );

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let sink_stdout = state.events.clone();
    let sink_stderr = state.events.clone();
    let discord_stdout = (state.discord.clone(), state.settings.clone());
    let id_stdout = instance_id.to_string();
    let id_stderr = instance_id.to_string();
    let running_stdout = running.clone();
    let running_stderr = running.clone();

    if let Some(stdout) = stdout {
        tokio::spawn(async move {
            let (discord, settings) = discord_stdout;
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                discord.on_game_log(&settings, &id_stdout, &line);
                running_stdout.push_log(&id_stdout, line.clone(), "stdout").await;
                let _ = sink_stdout.emit(
                    Event::raw(
                        EventName::LaunchLog,
                        LaunchLogEvent {
                            instance_id: id_stdout.clone(),
                            line,
                            stream: "stdout".to_string(),
                        },
                    )
                    .expect("LaunchLogEvent serializes"),
                );
            }
        });
    }
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                running_stderr.push_log(&id_stderr, line.clone(), "stderr").await;
                let _ = sink_stderr.emit(
                    Event::raw(
                        EventName::LaunchLog,
                        LaunchLogEvent {
                            instance_id: id_stderr.clone(),
                            line,
                            stream: "stderr".to_string(),
                        },
                    )
                    .expect("LaunchLogEvent serializes"),
                );
            }
        });
    }

    let sink_exit = state.events.clone();
    let id_exit = instance_id.to_string();
    let running_exit = running.clone();
    let instances_exit = instances.clone();
    let post_exit_command = instance.post_exit_command.clone();
    let hook_instance = instance.clone();
    let hook_java_path = plan.java_path.clone();
    let hook_java_args = plan.args.clone();
    tokio::spawn(async move {
        let status = child.wait().await.ok();
        running_exit.mark_stopped(&id_exit).await;
        let code = status.and_then(|s| s.code());
        let end = running_exit.record_exit(&id_exit, started_at, pid, code).await;

        let elapsed = now_secs().saturating_sub(started_at);
        if elapsed > 0 {
            let _ = instances_exit.add_playtime(&id_exit, elapsed).await;
        }

        if let Some(command) = &post_exit_command {
            run_hook_command(&hook_instance, &hook_java_path, &hook_java_args, command).await;
        }

        let _ = sink_exit.emit(
            Event::raw(
                EventName::LaunchExit,
                LaunchExitEvent {
                    instance_id: id_exit,
                    code,
                    crashed: end.crashed,
                },
            )
            .expect("LaunchExitEvent serializes"),
        );
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::minecraft::MinecraftProfile;
    use serde_json::json;

    fn temp_root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("arcane-plan-test-{}-{name}", std::process::id()))
    }

    /// A representative vanilla 1.20-style profile: structured arguments
    /// (the modern shape), one allowed library, an everywhere-excluded
    /// library and conditional JVM args exercising the rule engine (a bare
    /// `allow` rule matches every platform; a bare `disallow` excludes
    /// everywhere — matching rules flip `rules_allow`'s verdict), plus
    /// memory / java-args / window extras and Quick Play server targeting.
    /// The expected element list below is the byte-identical output the
    /// Tauri backend produced for this exact fixture (the
    /// `build_launch_plan` code is a byte-identical copy in both trees; see
    /// MIGRATION.md Phase 1 acceptance).
    fn sample_version() -> VersionDetail {
        serde_json::from_value(json!({
            "id": "1.20.4",
            "mainClass": "net.minecraft.client.main.Main",
            "arguments": {
                "jvm": [
                    "-Djava.library.path=${natives_directory}",
                    {"rules": [{"action": "disallow"}], "value": "-Dblocked=always"},
                    {"rules": [{"action": "allow"}], "value": "-Dallowed=everywhere"},
                    "-cp",
                    "${classpath}"
                ],
                "game": [
                    "--username", "${auth_player_name}",
                    "--version", "${version_name}",
                    "--gameDir", "${game_directory}",
                    "--assetsDir", "${assets_root}",
                    "--assetIndex", "${assets_index_name}",
                    "--uuid", "${auth_uuid}",
                    "--accessToken", "${auth_access_token}",
                    "--clientId", "${clientid}",
                    "--xuid", "${auth_xuid}",
                    "--userType", "${user_type}",
                    "--versionType", "${version_type}"
                ]
            },
            "libraries": [
                {
                    "name": "com.example:ok:1.0",
                    "downloads": {"artifact": {
                        "path": "com/example/ok/1.0/ok-1.0.jar",
                        "url": "https://example.com/com/example/ok/1.0/ok-1.0.jar",
                        "sha1": "abc",
                        "size": 1
                    }}
                },
                {
                    "name": "com.example:otheros:1.0",
                    "downloads": {"artifact": {"path": "com/example/otheros/1.0/otheros-1.0.jar", "url": "https://example.com/x.jar"}},
                    "rules": [{"action": "disallow"}]
                }
            ],
            "assetIndex": {"id": "21", "sha1": "x", "size": 1, "totalSize": 10, "url": "https://example.com/21.json"}
        }))
        .expect("test version fixture is valid")
    }

    fn sample_instance(root: &Path) -> MinecraftInstance {
        serde_json::from_value(json!({
            "id": "demo", "name": "Demo", "minecraft_version": "1.20.4",
            "loader": "vanilla", "loader_version": null,
            "game_dir": root.join("instances/demo").to_string_lossy(),
            "java_path": null, "java_args": "-Darcane.demo=true -Xss1m",
            "memory_mb": 4096, "window_width": 1280, "window_height": 720,
            "icon": null, "created_at": 0, "last_played": null, "imported": false
        }))
        .expect("test instance fixture is valid")
    }

    fn sample_session() -> AuthSession {
        AuthSession {
            session_type: SessionType::Microsoft,
            profile: MinecraftProfile {
                id: "6f1e0cb0-9c71-4b56-8c56-4b8cbea1efa3".to_string(),
                name: "Steve".to_string(),
            },
            access_token: Some("TOKEN".to_string()),
            refresh_token: None,
            expires_at: None,
        }
    }

    fn prepare_root(paths: &AppPaths) {
        let client_jar = paths.versions_meta_dir().join("1.20.4").join("client.jar");
        std::fs::create_dir_all(client_jar.parent().unwrap()).expect("client dir");
        std::fs::write(&client_jar, b"jar").expect("client.jar");
        std::fs::create_dir_all(paths.libraries_dir().join("com/example/ok/1.0")).expect("lib dir");
        std::fs::write(paths.libraries_dir().join("com/example/ok/1.0/ok-1.0.jar"), b"jar").expect("lib jar");
    }

    #[test]
    fn launch_plan_has_the_exact_expected_command_line() {
        let root = temp_root("full");
        let _ = std::fs::remove_dir_all(&root);
        let paths = AppPaths::new_root(root.clone()).expect("temp root");
        prepare_root(&paths);

        let instance = sample_instance(&root);
        let session = sample_session();
        let natives = root.join("versions").join("1.20.4").join("natives");
        let plan = build_launch_plan(
            &instance, &sample_version(), &session, "java", &natives, &paths, Some("play.example.net:25565"),
        )
        .expect("plan builds");

        // Same assembly as the builder, verified against the fixture above.
        let sep = if cfg!(target_os = "windows") { ";" } else { ":" };
        let classpath = [
            paths.libraries_dir().join("com/example/ok/1.0/ok-1.0.jar"),
            paths.versions_meta_dir().join("1.20.4").join("client.jar"),
        ]
        .map(|p| p.to_string_lossy().into_owned())
        .join(sep);
        let natives_str = natives.to_string_lossy().into_owned();
        let assets_root = paths.assets_dir().to_string_lossy().into_owned();
        let game_dir = instance.game_dir.clone();

        let expected_args = vec![
            format!("-Djava.library.path={natives_str}"),
            "-Dallowed=everywhere".to_string(),
            "-cp".to_string(),
            classpath,
            "-Xmx4096M".to_string(),
            "-Xms2048M".to_string(),
            "-Darcane.demo=true".to_string(),
            "-Xss1m".to_string(),
            "net.minecraft.client.main.Main".to_string(),
            "--username".to_string(),
            "Steve".to_string(),
            "--version".to_string(),
            "1.20.4".to_string(),
            "--gameDir".to_string(),
            game_dir,
            "--assetsDir".to_string(),
            assets_root,
            "--assetIndex".to_string(),
            "21".to_string(),
            "--uuid".to_string(),
            "6f1e0cb0-9c71-4b56-8c56-4b8cbea1efa3".to_string(),
            "--accessToken".to_string(),
            "TOKEN".to_string(),
            "--clientId".to_string(),
            "arcane-launcher".to_string(),
            "--xuid".to_string(),
            "0".to_string(),
            "--userType".to_string(),
            "msa".to_string(),
            "--versionType".to_string(),
            "Arcane Launcher".to_string(),
            "--width".to_string(),
            "1280".to_string(),
            "--height".to_string(),
            "720".to_string(),
            "--quickPlayMultiplayer".to_string(),
            "play.example.net:25565".to_string(),
        ];
        assert_eq!(plan.args, expected_args);
        assert_eq!(plan.java_path, "java");
        assert_eq!(plan.env, Vec::<(String, String)>::new());
        assert_eq!(plan.working_dir.to_string_lossy(), instance.game_dir);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_client_jar_is_a_repairable_error() {
        let root = temp_root("broken");
        let _ = std::fs::remove_dir_all(&root);
        let paths = AppPaths::new_root(root.clone()).expect("temp root");
        // No client.jar written.

        let instance = sample_instance(&root);
        let natives = root.join("natives");
        let err = build_launch_plan(&instance, &sample_version(), &sample_session(), "java", &natives, &paths, None)
            .expect_err("no client.jar means no plan");
        assert_eq!(err.message, "The Minecraft client is missing. Repair this instance before launching.");

        let _ = std::fs::remove_dir_all(&root);
    }
}
