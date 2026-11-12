use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OfflineModePreference {
    #[default]
    Automatic,
    AlwaysOffline,
}

/// How friends can join from the Discord status while you're on a public
/// server. Discord allows one or the other on a status, never both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscordJoinMode {
    /// Discord's own "Ask to Join": friends ask, you accept in the launcher.
    #[default]
    Ask,
    /// A "Join server" link button anyone viewing the profile can use.
    Button,
}

/// Which graphics card Minecraft runs on, for PCs with more than one
/// (typically a laptop with integrated + dedicated graphics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuPreference {
    /// Leave it to Windows / the graphics driver. Anything the launcher set
    /// earlier is removed again.
    #[default]
    Auto,
    /// The dedicated GPU (what Windows calls "High performance").
    HighPerformance,
    /// The integrated GPU: cooler, quieter, longer battery.
    PowerSaving,
}

/// How far the launcher goes on its own when a new version is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    /// Download in the background, then install after a short countdown
    /// whenever no game or download is running.
    #[default]
    Automatic,
    /// Download in the background, but only install when the player says
    /// so (or closes the launcher, if `install_on_close` is on).
    Ask,
    /// Only say a new version exists; nothing is downloaded until asked.
    Notify,
    /// No background checks at all; "Check for updates" still works.
    Off,
}

/// Which releases this install follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    /// Stable releases plus pre-releases published with `--channel beta`,
    /// whichever is newer.
    Beta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatePreferences {
    pub mode: UpdateMode,
    pub channel: UpdateChannel,
    /// "Skip this version": background checks ignore exactly this version
    /// (a newer one is offered normally). Manual checks still show it.
    pub skipped_version: Option<String>,
    /// Minutes between background checks while the launcher is open.
    /// 0 = only once, shortly after startup.
    pub check_interval_minutes: u32,
    /// Closing the launcher installs a downloaded update (no restart).
    pub install_on_close: bool,
    /// Show the release notes on the first start after an update.
    pub show_whats_new: bool,
}

impl Default for UpdatePreferences {
    fn default() -> Self {
        Self {
            mode: UpdateMode::Automatic,
            channel: UpdateChannel::Stable,
            skipped_version: None,
            check_interval_minutes: 30,
            install_on_close: true,
            show_whats_new: true,
        }
    }
}

impl UpdatePreferences {
    pub const MIN_INTERVAL_MINUTES: u32 = 15;
    pub const MAX_INTERVAL_MINUTES: u32 = 24 * 60;

    /// Keeps a hand-edited or out-of-range interval from hammering the
    /// update server (or never checking again).
    pub fn sanitized(mut self) -> Self {
        if self.check_interval_minutes != 0 {
            self.check_interval_minutes = self
                .check_interval_minutes
                .clamp(Self::MIN_INTERVAL_MINUTES, Self::MAX_INTERVAL_MINUTES);
        }
        self
    }
}

/// What the game recorder records as sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingAudio {
    /// Only Minecraft's own sound (Windows' per-process loopback) — no
    /// Discord calls, music players or launcher sounds.
    #[default]
    Game,
    /// Everything the PC plays through the default output device.
    System,
    /// Video only.
    Off,
}

/// Video bitrate, scaled to the window size and frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingQuality {
    Low,
    #[default]
    Medium,
    High,
}

impl RecordingQuality {
    /// Bits per pixel per frame for H.264 — ~12 Mbps at 1080p60 for
    /// Medium, in line with what ShadowPlay / OBS presets use.
    pub fn bits_per_pixel(self) -> f64 {
        match self {
            Self::Low => 0.05,
            Self::Medium => 0.1,
            Self::High => 0.16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    /// The global shortcut that starts/stops a recording while a game runs.
    pub hotkey_enabled: bool,
    /// Tauri global-shortcut syntax, e.g. "Ctrl+Alt+R".
    pub hotkey: String,
    pub fps: u32,
    pub quality: RecordingQuality,
    pub audio: RecordingAudio,
    /// Output height (720, 1080, 1440 or 2160); the game is scaled to fit
    /// with black bars if its shape differs, so resizing or F11 never
    /// changes the video's size.
    pub resolution: u32,
    /// Game/system sound level, percent (0-200).
    pub game_volume: u32,
    pub mic_enabled: bool,
    /// WASAPI endpoint id; `None` = the Windows default microphone.
    pub mic_device: Option<String>,
    /// Percent (0-200).
    pub mic_volume: u32,
    /// Keeps the last `replay_seconds` of play buffered while a game runs,
    /// saved with `replay_hotkey`.
    pub replay_enabled: bool,
    pub replay_seconds: u32,
    pub replay_hotkey: String,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            hotkey_enabled: true,
            hotkey: Self::DEFAULT_HOTKEY.to_string(),
            fps: 60,
            quality: RecordingQuality::Medium,
            audio: RecordingAudio::Game,
            resolution: 1080,
            game_volume: 100,
            mic_enabled: false,
            mic_device: None,
            mic_volume: 100,
            replay_enabled: false,
            replay_seconds: 30,
            replay_hotkey: Self::DEFAULT_REPLAY_HOTKEY.to_string(),
        }
    }
}

impl RecordingSettings {
    pub const DEFAULT_HOTKEY: &'static str = "Ctrl+Alt+R";
    pub const DEFAULT_REPLAY_HOTKEY: &'static str = "Ctrl+Alt+S";
    pub const RESOLUTIONS: [u32; 4] = [720, 1080, 1440, 2160];
    pub const MIN_REPLAY_SECONDS: u32 = 15;
    pub const MAX_REPLAY_SECONDS: u32 = 300;

    pub fn sanitized(mut self) -> Self {
        self.fps = if self.fps <= 30 { 30 } else { 60 };
        if self.hotkey.trim().is_empty() {
            self.hotkey = Self::DEFAULT_HOTKEY.to_string();
        }
        if self.replay_hotkey.trim().is_empty() {
            self.replay_hotkey = Self::DEFAULT_REPLAY_HOTKEY.to_string();
        }
        if !Self::RESOLUTIONS.contains(&self.resolution) {
            self.resolution = 1080;
        }
        self.game_volume = self.game_volume.min(200);
        self.mic_volume = self.mic_volume.min(200);
        self.replay_seconds = self
            .replay_seconds
            .clamp(Self::MIN_REPLAY_SECONDS, Self::MAX_REPLAY_SECONDS);
        self.mic_device = self.mic_device.filter(|d| !d.trim().is_empty());
        self
    }

    /// 16:9 output size for `resolution`.
    pub fn output_size(&self) -> (u32, u32) {
        let height = self.resolution;
        ((height * 16 / 9 + 1) & !1, height)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default)]
    pub offline_mode: OfflineModePreference,
    #[serde(default)]
    pub curseforge_api_key: Option<String>,
    #[serde(default)]
    pub microsoft_client_id: Option<String>,
    #[serde(default)]
    pub default_java_path: Option<String>,
    #[serde(default)]
    pub auto_install_required_dependencies: bool,
    #[serde(default)]
    pub selected_instance_id: Option<String>,
    #[serde(default)]
    pub discord_rpc_enabled: bool,
    #[serde(default)]
    pub discord_client_id: Option<String>,
    #[serde(default)]
    pub discord_join_mode: DiscordJoinMode,
    /// Named so the "shown by default" behavior survives loading an older
    /// settings.json that predates this field: serde fills a missing field
    /// with its type's own default (`false` for bool), so a field meaning
    /// "hidden" rather than "enabled" defaults correctly either way.
    #[serde(default)]
    pub background_disabled: bool,
    /// Percent (0-100) of dark tint drawn over the background image so page
    /// content stays legible.
    #[serde(default)]
    pub background_overlay_opacity: u8,
    /// Off (the default): an instance becomes playable as soon as the
    /// essential game files are in, and music / music discs / non-English
    /// languages (~80% of a modern version's assets) finish downloading in
    /// the background. On: everything downloads before the first launch.
    #[serde(default)]
    pub full_assets_before_launch: bool,
    #[serde(default)]
    pub updates: UpdatePreferences,
    #[serde(default)]
    pub gpu_preference: GpuPreference,
    #[serde(default)]
    pub recording: RecordingSettings,
    /// October's " Haunting ..." Discord status with pumpkin art. Stored
    /// as "disabled" so it's on for settings files that predate it.
    #[serde(default)]
    pub discord_spooky_disabled: bool,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            offline_mode: OfflineModePreference::Automatic,
            curseforge_api_key: None,
            microsoft_client_id: None,
            default_java_path: None,
            auto_install_required_dependencies: true,
            selected_instance_id: None,
            discord_rpc_enabled: true,
            discord_client_id: None,
            discord_join_mode: DiscordJoinMode::Ask,
            background_disabled: false,
            background_overlay_opacity: 70,
            full_assets_before_launch: false,
            updates: UpdatePreferences::default(),
            gpu_preference: GpuPreference::Auto,
            recording: RecordingSettings::default(),
            discord_spooky_disabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_file_gets_default_update_preferences() {
        let s: LauncherSettings = serde_json::from_str(r#"{"offline_mode":"automatic"}"#).unwrap();
        assert_eq!(s.updates.mode, UpdateMode::Automatic);
        assert_eq!(s.updates.channel, UpdateChannel::Stable);
        assert_eq!(s.updates.skipped_version, None);
        assert_eq!(s.updates.check_interval_minutes, 30);
        assert!(s.updates.install_on_close);
        assert!(s.updates.show_whats_new);
        assert_eq!(s.gpu_preference, GpuPreference::Auto);
        assert!(s.recording.hotkey_enabled);
        assert_eq!(s.recording.hotkey, RecordingSettings::DEFAULT_HOTKEY);
        assert_eq!(s.recording.fps, 60);
        assert_eq!(s.recording.audio, RecordingAudio::Game);
        assert_eq!(s.recording.resolution, 1080);
        assert!(!s.recording.replay_enabled);
        assert_eq!(
            s.recording.replay_hotkey,
            RecordingSettings::DEFAULT_REPLAY_HOTKEY
        );
    }

    #[test]
    fn recording_values_are_clamped() {
        let r = RecordingSettings {
            resolution: 999,
            replay_seconds: 5,
            mic_volume: 900,
            mic_device: Some(" ".into()),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(r.resolution, 1080);
        assert_eq!(r.replay_seconds, RecordingSettings::MIN_REPLAY_SECONDS);
        assert_eq!(r.mic_volume, 200);
        assert_eq!(r.mic_device, None);
        assert_eq!(
            RecordingSettings {
                resolution: 720,
                ..Default::default()
            }
            .output_size(),
            (1280, 720)
        );
        assert_eq!(
            RecordingSettings {
                resolution: 1440,
                ..Default::default()
            }
            .output_size(),
            (2560, 1440)
        );
    }

    #[test]
    fn recording_fps_snaps_to_30_or_60() {
        let r = |fps| {
            RecordingSettings {
                fps,
                ..Default::default()
            }
            .sanitized()
            .fps
        };
        assert_eq!(r(0), 30);
        assert_eq!(r(30), 30);
        assert_eq!(r(45), 60);
        assert_eq!(r(144), 60);
    }

    #[test]
    fn partial_update_preferences_keep_other_defaults() {
        let s: LauncherSettings = serde_json::from_str(r#"{"updates":{"mode":"notify"}}"#).unwrap();
        assert_eq!(s.updates.mode, UpdateMode::Notify);
        assert!(s.updates.install_on_close);
    }

    #[test]
    fn interval_is_clamped_but_zero_means_startup_only() {
        let p = |m| {
            UpdatePreferences {
                check_interval_minutes: m,
                ..Default::default()
            }
            .sanitized()
        };
        assert_eq!(p(0).check_interval_minutes, 0);
        assert_eq!(p(1).check_interval_minutes, 15);
        assert_eq!(p(100_000).check_interval_minutes, 1440);
        assert_eq!(p(60).check_interval_minutes, 60);
    }
}
