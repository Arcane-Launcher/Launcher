use std::io::Read;

use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::instance::MinecraftInstance;
use crate::models::minecraft::{
    ArgumentValue, ArgumentValueInner, Arguments, Library, LibraryArtifact, LibraryDownloads,
    VersionDetail,
};
use crate::services::download::manager::ExpectedHash;
use crate::services::minecraft::libraries::{
    maven_coordinate_to_path, resolve_library_artifact, rules_allow,
};
use crate::services::minecraft::MinecraftEngine;

/// Forge and NeoForge (1.13+/all NeoForge versions) ship a Gradle
/// build-system installer rather than a pre-baked launcher profile like
/// Fabric/Quilt provide. Rather than reimplementing that installer's
/// arbitrary "processor" pipeline ourselves, this follows the same approach
/// PrismLauncher uses: ForgeWrapper (https://github.com/PrismLauncher/ForgeWrapper)
/// is a small jar that, when set as the launch main class with a few
/// `-Dforgewrapper.*` system properties, runs the real installer's own
/// processors itself on first launch and then chainloads into Forge/NeoForge
/// normally. See PrismLauncher's `meta` repo, `generate_forge.py` /
/// `generate_neoforge.py`, for the reference this mirrors.
pub struct ForgeLikeConfig {
    pub loader_name: &'static str,
    pub group: &'static str,
    pub maven_base: &'static str,
    pub artifact_id: &'static str,
    pub add_fml_args: bool,
}

#[derive(Deserialize)]
struct InstallProfile {
    #[serde(rename = "json")]
    json_path: Option<String>,
    #[serde(default)]
    libraries: Vec<Library>,
}

const FORGEWRAPPER_COORDINATE: &str = "io.github.zekerzhayard:ForgeWrapper:prism-2026-08-01";
const FORGEWRAPPER_SHA1: &str = "852b7e59748da1512d40e38407eadb1f0031a996";
const FORGEWRAPPER_SIZE: u64 = 29800;
const FORGEWRAPPER_MAVEN: &str = "https://files.prismlauncher.org/maven/";

fn forgewrapper_library() -> Library {
    let relative = maven_coordinate_to_path(FORGEWRAPPER_COORDINATE).expect("valid coordinate");
    Library {
        name: FORGEWRAPPER_COORDINATE.to_string(),
        downloads: Some(LibraryDownloads {
            artifact: Some(LibraryArtifact {
                path: Some(relative.clone()),
                url: format!("{FORGEWRAPPER_MAVEN}{relative}"),
                sha1: Some(FORGEWRAPPER_SHA1.to_string()),
                size: Some(FORGEWRAPPER_SIZE),
            }),
            classifiers: None,
        }),
        rules: None,
        natives: None,
        url: None,
    }
}

fn is_log4j(coordinate: &str) -> bool {
    coordinate.starts_with("org.apache.logging.log4j:")
}

fn extract_simple_game_args(arguments: &Option<Arguments>) -> Vec<String> {
    let Some(arguments) = arguments else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for arg in &arguments.game {
        match arg {
            ArgumentValue::Simple(s) => out.push(s.clone()),
            ArgumentValue::Conditional { rules, value } => {
                if rules_allow(rules) {
                    match value {
                        ArgumentValueInner::Single(s) => out.push(s.clone()),
                        ArgumentValueInner::Multiple(list) => out.extend(list.iter().cloned()),
                    }
                }
            }
        }
    }
    out
}

/// Installs Forge/NeoForge via the ForgeWrapper approach and returns the
/// version key to hand to [`MinecraftEngine::ensure_installed`].
///
/// `long_version` is the maven version segment for the main artifact (e.g.
/// `1.20.1-47.3.0` for Forge, or just `21.1.65` for NeoForge, which doesn't
/// prefix with the Minecraft version). `loader_version_display` is what
/// shows up in the stored version id and any `--fml.forgeVersion` arg.
pub async fn install(
    instance: &MinecraftInstance,
    engine: &MinecraftEngine,
    config: &ForgeLikeConfig,
    long_version: &str,
    loader_version_display: &str,
) -> AppResult<String> {
    let mc_version = instance.minecraft_version.clone();
    let id = format!(
        "{}-loader-{loader_version_display}-{mc_version}",
        config.loader_name
    );

    if engine
        .manifest
        .resolve_effective_version_offline(&id)
        .await
        .is_some()
    {
        return Ok(id);
    }

    let installer_coordinate = format!(
        "{}:{}:{long_version}:installer",
        config.group, config.artifact_id
    );
    let installer_relative = maven_coordinate_to_path(&installer_coordinate).ok_or_else(|| {
        AppError::new(
            format!("Couldn't prepare {}.", config.loader_name),
            format!("bad installer coordinate {installer_coordinate}"),
        )
    })?;
    let installer_url = format!("{}{installer_relative}", config.maven_base);

    let temp_path = engine
        .paths
        .downloads_temp_dir()
        .join(format!("{}.jar", Uuid::new_v4()));
    engine
        .downloads
        .download_file(
            &format!("{} installer", config.loader_name),
            &installer_url,
            temp_path.clone(),
            ExpectedHash {
                sha1: None,
                sha256: None,
                sha512: None,
            },
            None,
        )
        .await
        .map_err(|_| {
            AppError::new(
                format!(
                    "{} {loader_version_display} isn't available for Minecraft {mc_version}.",
                    config.loader_name
                ),
                format!("installer download failed: {installer_url}"),
            )
        })?;

    let bytes = tokio::fs::read(&temp_path).await?;
    let _ = tokio::fs::remove_file(&temp_path).await;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .map_err(|e| AppError::new("This installer file is corrupted.", e.to_string()))?;

    let profile: InstallProfile = {
        let mut entry = archive.by_name("install_profile.json").map_err(|_| {
            AppError::new(
                format!(
                    "This doesn't look like a valid {} installer.",
                    config.loader_name
                ),
                "install_profile.json missing from installer jar",
            )
        })?;
        let mut buf = String::new();
        entry.read_to_string(&mut buf)?;
        serde_json::from_str(&buf).map_err(|e| {
            AppError::new(
                "This installer's manifest could not be read.",
                e.to_string(),
            )
        })?
    };

    let version_json_name = profile
        .json_path
        .as_deref()
        .unwrap_or("/version.json")
        .trim_start_matches('/')
        .to_string();
    let installer_version: VersionDetail = {
        let mut entry = archive.by_name(&version_json_name).map_err(|_| {
            AppError::new(
                format!(
                    "This {} installer is missing its version metadata.",
                    config.loader_name
                ),
                format!("{version_json_name} missing from installer jar"),
            )
        })?;
        let mut buf = String::new();
        entry.read_to_string(&mut buf)?;
        serde_json::from_str(&buf).map_err(|e| {
            AppError::new(
                "This installer's version metadata could not be read.",
                e.to_string(),
            )
        })?
    };
    drop(archive);

    // The installer jar itself is stored under its own maven coordinate so
    // ForgeWrapper can find it later via `-Dforgewrapper.installer=...`.
    let installer_lib_dest = engine.paths.libraries_dir().join(&installer_relative);
    if !installer_lib_dest.exists() {
        if let Some(parent) = installer_lib_dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&installer_lib_dest, &bytes).await?;
    }

    // Forge's own patch/data files from install_profile.json: these must be
    // present on disk (by maven coordinate) for ForgeWrapper to consume, but
    // are never added to the launch classpath themselves.
    for lib in profile.libraries.iter().filter(|l| !is_log4j(&l.name)) {
        if let Some(resolved) = resolve_library_artifact(lib) {
            let dest = engine.paths.libraries_dir().join(&resolved.relative_path);
            if !dest.exists() {
                let _ = engine
                    .downloads
                    .download_file(
                        &lib.name,
                        &resolved.url,
                        dest,
                        ExpectedHash {
                            sha1: resolved.sha1,
                            sha256: None,
                            sha512: None,
                        },
                        None,
                    )
                    .await;
            }
        }
    }

    // Classpath libraries: ForgeWrapper itself, plus whatever the installer's
    // bundled version.json declares (with the bare main artifact — no
    // classifier — renamed to classifier "launcher", matching PrismLauncher).
    let mut classpath_libraries = vec![forgewrapper_library()];
    let bare_prefix = format!("{}:{}:", config.group, config.artifact_id);
    for mut lib in installer_version
        .libraries
        .into_iter()
        .filter(|l| !is_log4j(&l.name))
    {
        if lib.name.starts_with(&bare_prefix) && lib.name.matches(':').count() == 2 {
            lib.name = format!("{}:launcher", lib.name);
            if let Some(new_relative) = maven_coordinate_to_path(&lib.name) {
                let url = format!("{}{new_relative}", config.maven_base);
                match &mut lib.downloads {
                    Some(downloads) => {
                        downloads.artifact = Some(LibraryArtifact {
                            path: Some(new_relative),
                            url,
                            sha1: None,
                            size: None,
                        });
                    }
                    None => {
                        lib.downloads = Some(LibraryDownloads {
                            artifact: Some(LibraryArtifact {
                                path: Some(new_relative),
                                url,
                                sha1: None,
                                size: None,
                            }),
                            classifiers: None,
                        });
                    }
                }
            }
        }
        classpath_libraries.push(lib);
    }

    let libraries_dir_str = engine.paths.libraries_dir().to_string_lossy().to_string();
    let client_jar_str = engine
        .paths
        .versions_meta_dir()
        .join(crate::paths::sanitize_component(&mc_version))
        .join("client.jar")
        .to_string_lossy()
        .to_string();

    let jvm_args = vec![
        ArgumentValue::Simple(format!("-Dforgewrapper.librariesDir={libraries_dir_str}")),
        ArgumentValue::Simple(format!(
            "-Dforgewrapper.installer={}",
            installer_lib_dest.to_string_lossy()
        )),
        ArgumentValue::Simple(format!("-Dforgewrapper.minecraft={client_jar_str}")),
    ];

    let mut mc_args =
        "--username ${auth_player_name} --version ${version_name} --gameDir ${game_directory} \
        --assetsDir ${assets_root} --assetIndex ${assets_index_name} --uuid ${auth_uuid} \
        --accessToken ${auth_access_token} --userType ${user_type} --versionType ${version_type}"
            .to_string();
    for arg in extract_simple_game_args(&installer_version.arguments) {
        mc_args.push(' ');
        mc_args.push_str(&arg);
    }
    if config.add_fml_args {
        if !mc_args.contains("--fml.forgeGroup") {
            mc_args.push_str(&format!(" --fml.forgeGroup {}", config.group));
        }
        if !mc_args.contains("--fml.forgeVersion") {
            mc_args.push_str(&format!(" --fml.forgeVersion {loader_version_display}"));
        }
        if !mc_args.contains("--fml.mcVersion") {
            mc_args.push_str(&format!(" --fml.mcVersion {mc_version}"));
        }
    }

    let version_detail = VersionDetail {
        id: id.clone(),
        main_class: "io.github.zekerzhayard.forgewrapper.installer.Main".to_string(),
        arguments: Some(Arguments {
            game: Vec::new(),
            jvm: jvm_args,
        }),
        minecraft_arguments: Some(mc_args),
        libraries: classpath_libraries,
        downloads: None,
        asset_index: None,
        assets: None,
        java_version: None,
        inherits_from: Some(mc_version.clone()),
        kind: None,
    };

    let json_bytes = serde_json::to_vec(&version_detail)?;
    engine
        .manifest
        .store_version_detail(&id, &json_bytes)
        .await?;
    Ok(id)
}
