use serde::{Deserialize, Serialize};

use super::common::{ModLoaderKind, ModSource};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinecraftInstance {
    pub id: String,
    pub name: String,
    pub minecraft_version: String,
    pub loader: ModLoaderKind,
    pub loader_version: Option<String>,
    pub game_dir: String,
    pub java_path: Option<String>,
    pub java_args: Option<String>,
    /// JVM max heap size (`-Xmx`) in megabytes. `None` means "let Java pick
    /// its own default" rather than a hidden fixed value — instances
    /// created by this launcher always get one (see
    /// `services::system::default_instance_memory_mb`); only pre-existing
    /// imported instances may lack it until a user sets one explicitly.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Custom launch window size in pixels. `None` on either means "use
    /// Minecraft's own default" — never a hidden fixed value.
    #[serde(default)]
    pub window_width: Option<u32>,
    #[serde(default)]
    pub window_height: Option<u32>,
    /// A data: URI (small, embedded directly) or a remote URL (e.g. a
    /// modpack's own icon from its source) — never a local file path, so it
    /// always renders regardless of the webview's asset-protocol scope.
    pub icon: Option<String>,
    pub created_at: u64,
    pub last_played: Option<u64>,
    /// Total accumulated play time in seconds, summed across every session
    /// as it ends (see `launch://exit` handling) — not an estimate.
    #[serde(default)]
    pub total_playtime_secs: u64,
    /// True when the instance was created by pointing at a folder that
    /// already contained Minecraft (a pre-existing installation), rather
    /// than one this launcher installed itself.
    pub imported: bool,
    /// Set only for instances created from a modpack, so a later "check for
    /// update" can compare against the pack's latest published version.
    #[serde(default)]
    pub modpack_source: Option<ModSource>,
    #[serde(default)]
    pub modpack_project_id: Option<String>,
    #[serde(default)]
    pub modpack_version_id: Option<String>,
    /// When on, every mod/resource pack/shader with a compatible newer
    /// version gets updated automatically right before this instance
    /// launches, instead of the player having to check the Installed Mods
    /// page themselves.
    #[serde(default)]
    pub auto_update_content: bool,
    /// A free-form label used to cluster instances together in the
    /// instance list (e.g. "Vanilla", "Modded", "Testing"). `None`/empty
    /// means ungrouped. Deliberately just a flat string, not a hierarchy —
    /// mirrors how PrismLauncher's own instance groups work.
    #[serde(default)]
    pub group: Option<String>,
    /// Freeform text the player can leave on an instance (why it exists,
    /// what's special about its setup) — never read or used by the
    /// launcher itself.
    #[serde(default)]
    pub notes: Option<String>,
    /// Run through the OS shell (with `INST_*` environment variables set,
    /// see `services::minecraft::launch::run_hook_command`) right before
    /// Minecraft is spawned. A failure here is logged but never blocks the
    /// actual launch.
    #[serde(default)]
    pub pre_launch_command: Option<String>,
    /// Same as `pre_launch_command`, but run after the game process exits.
    #[serde(default)]
    pub post_exit_command: Option<String>,
    /// Pinned to the top of the instance list, ahead of sort order.
    #[serde(default)]
    pub favorite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInstanceRequest {
    pub name: String,
    pub minecraft_version: String,
    pub loader: ModLoaderKind,
    pub loader_version: Option<String>,
    /// When set, import this existing folder instead of creating a fresh one.
    pub existing_game_dir: Option<String>,
    pub java_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    pub severity: ValidationSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceValidationReport {
    pub ready: bool,
    pub issues: Vec<ValidationIssue>,
}
