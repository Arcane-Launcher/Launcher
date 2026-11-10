//! Game recorder: records the running Minecraft window (and its sound,
//! optionally with the microphone) to `<instance>/recordings/*.mp4`, keeps
//! an instant-replay buffer while playing, and trims / compresses / copies
//! saved clips. Started from the launcher or with global shortcuts in game.
//! Windows only; elsewhere `is_supported()` is false and the UI hides it.

#[cfg(windows)]
mod win;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::events::{Event, EventName, EventSink};
use crate::models::instance::MinecraftInstance;
use crate::models::settings::{RecordingAudio, RecordingSettings};
use crate::paths::safe_join;
use crate::state::AppState;

pub const STATE_EVENT: &str = "recorder://state";
pub const SAVED_EVENT: &str = "recorder://saved";

/// Global OS shortcut registration — replaces `tauri_plugin_global_shortcut`.
/// The desktop shell owns a real implementation (registered only while a
/// game runs, exactly as before); the recorder never touches the OS itself.
pub trait Hotkeys: Send + Sync {
    fn unregister(&self, hotkey: &str) -> Result<(), String>;
    /// Registers the combo; `callback` fires on key press. Err (usually
    /// another app owns the combination) must not affect other combos.
    fn register(&self, hotkey: &str, callback: Box<dyn Fn() + Send + Sync>) -> Result<(), String>;
}

/// No-op implementation for contexts without windowing (tests, and
/// non-Windows platforms where recording is unsupported anyway — nothing
/// ever calls `sync_hotkey` there because no game can be running).
#[derive(Default, Clone, Copy)]
pub struct DisabledHotkeys;

impl Hotkeys for DisabledHotkeys {
    fn unregister(&self, _hotkey: &str) -> Result<(), String> {
        Ok(())
    }
    fn register(&self, _hotkey: &str, _callback: Box<dyn Fn() + Send + Sync>) -> Result<(), String> {
        Err("no hotkey service in this context".into())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecorderStatus {
    pub supported: bool,
    pub recording: bool,
    /// The game being captured (recording and/or replay buffer).
    pub instance_id: Option<String>,
    /// Unix seconds the current recording started.
    pub started_at: Option<u64>,
    pub replay_active: bool,
    pub replay_seconds: Option<u32>,
    /// A sound source that was asked for but couldn't be captured.
    pub audio_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingInfo {
    pub filename: String,
    pub path: String,
    pub recorded_at: u64,
    pub size_bytes: u64,
}

/// Sent when a recording or replay is saved, or fails.
#[derive(Debug, Clone, Serialize)]
pub struct RecordingSaved {
    pub instance_id: String,
    pub files: Vec<RecordingInfo>,
    /// "recording" or "replay".
    pub kind: &'static str,
    /// "stopped" (by the player), "game_closed", "saved", or "failed".
    pub reason: &'static str,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Microphone {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[cfg(windows)]
type Engine = win::engine::Engine;

#[cfg(not(windows))]
struct Engine;

#[cfg(not(windows))]
#[allow(dead_code)]
impl Engine {
    fn is_closed(&self) -> bool {
        true
    }
    fn take_error(&self) -> Option<String> {
        None
    }
    fn is_recording(&self) -> bool {
        false
    }
    fn replay_active(&self) -> bool {
        false
    }
    fn start_recording(&self, _: PathBuf) {}
    fn stop_recording(&self) -> Result<Option<PathBuf>, String> {
        Ok(None)
    }
    fn save_replay(&self, _: &Path) -> Result<PathBuf, String> {
        Err("unsupported".into())
    }
    fn shutdown(&self) -> Option<Result<Option<PathBuf>, String>> {
        None
    }
}

struct Active {
    id: u64,
    instance_id: String,
    engine: Arc<Engine>,
    recording_started_at: Option<u64>,
    replay_seconds: Option<u32>,
    warning: Option<String>,
}

#[derive(Clone, Default)]
pub struct Recorder {
    active: Arc<Mutex<Option<Active>>>,
    /// Shortcuts currently registered with the OS.
    hotkeys: Arc<Mutex<Vec<String>>>,
    /// Bumped whenever the replay buffer should be (re)started, so an older
    /// "wait for the game window" loop gives up.
    replay_generation: Arc<AtomicU64>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn is_supported() -> bool {
    #[cfg(windows)]
    {
        win::is_supported()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn timestamp() -> String {
    #[cfg(windows)]
    {
        win::local_timestamp()
    }
    #[cfg(not(windows))]
    {
        now_secs().to_string()
    }
}

fn unsupported() -> AppError {
    AppError::new("Game recording isn't available on this system.", "recorder: unsupported platform")
}

pub fn recordings_dir(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("recordings")
}

fn file_info(path: &Path) -> Option<RecordingInfo> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(RecordingInfo {
        filename: path.file_name()?.to_string_lossy().to_string(),
        path: path.to_string_lossy().to_string(),
        recorded_at: metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        size_bytes: metadata.len(),
    })
}

/// `dir/stem.mp4`, or `dir/stem (2).mp4` etc. if taken.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.mp4"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).mp4")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}

impl Recorder {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Active>> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> RecorderStatus {
        let active = self.lock();
        let a = active.as_ref();
        RecorderStatus {
            supported: is_supported(),
            recording: a.is_some_and(|a| a.recording_started_at.is_some()),
            instance_id: a.map(|a| a.instance_id.clone()),
            started_at: a.and_then(|a| a.recording_started_at),
            replay_active: a.is_some_and(|a| a.replay_seconds.is_some()),
            replay_seconds: a.and_then(|a| a.replay_seconds),
            audio_warning: a.and_then(|a| a.warning.clone()),
        }
    }

    fn emit_status(&self, state: &AppState) {
        let _ = state.events.emit(
            Event::raw(EventName::RecorderState, self.status()).expect("RecorderStatus serializes"),
        );
    }

    fn engine(&self) -> Option<(Arc<Engine>, String)> {
        self.lock().as_ref().map(|a| (a.engine.clone(), a.instance_id.clone()))
    }

    /// The engine for `instance`, started if needed. Blocking.
    fn ensure_engine(
        &self,
        state: &AppState,
        instance: &MinecraftInstance,
        pid: u32,
        settings: &RecordingSettings,
        want_replay: bool,
    ) -> AppResult<Arc<Engine>> {
        if !is_supported() {
            return Err(unsupported());
        }
        {
            let active = self.lock();
            if let Some(a) = active.as_ref() {
                if a.instance_id == instance.id && (a.replay_seconds.is_some() || !want_replay) {
                    return Ok(a.engine.clone());
                }
                if a.recording_started_at.is_some() {
                    return Err(AppError::new("Another game is being recorded.", "recorder: busy"));
                }
            }
        }
        // A different game, or the replay buffer needs turning on.
        self.teardown(state, "stopped");

        #[cfg(windows)]
        let (engine, warning) = {
            use win::audio::{MixerConfig, SourceKind};
            use win::engine::{find_game_window, Engine as WinEngine, EngineConfig};
            use win::sink::VideoFormat;

            let hwnd = find_game_window(pid)
                .ok_or_else(|| AppError::new("Minecraft's window isn't open yet.", "recorder: no window"))?;
            let (width, height) = settings.output_size();
            let pixels_per_sec = f64::from(width) * f64::from(height) * f64::from(settings.fps);
            let bitrate = (pixels_per_sec * settings.quality.bits_per_pixel()).clamp(2_000_000.0, 80_000_000.0) as u32;
            let gain = |percent: u32| percent as f32 / 100.0;
            let main = match settings.audio {
                RecordingAudio::Game => Some((SourceKind::Game(pid), gain(settings.game_volume))),
                RecordingAudio::System => Some((SourceKind::System, gain(settings.game_volume))),
                RecordingAudio::Off => None,
            };
            let mic = settings.mic_enabled.then(|| (settings.mic_device.clone(), gain(settings.mic_volume)));
            let cache = state.paths.cache_dir().join("instant-replay");
            let engine = WinEngine::start(EngineConfig {
                hwnd,
                format: VideoFormat { width, height, fps: settings.fps, bitrate },
                audio: MixerConfig { main, mic },
                replay: want_replay.then_some((cache, settings.replay_seconds)),
            })
            .map_err(|e| AppError::new(e.clone(), format!("recorder start failed: {e}")))?;
            let warning = (!engine.warnings.is_empty()).then(|| engine.warnings.join(" "));
            (engine, warning)
        };
        #[cfg(not(windows))]
        let (engine, warning): (Engine, Option<String>) = {
            let _ = (pid, settings, RecordingAudio::Off);
            return Err(unsupported());
        };

        let engine = Arc::new(engine);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        *self.lock() = Some(Active {
            id,
            instance_id: instance.id.clone(),
            engine: engine.clone(),
            recording_started_at: None,
            replay_seconds: want_replay.then_some(settings.replay_seconds),
            warning,
        });

        // Notices the window closing (game quit, or Minecraft swapping its
        // window) or a capture failure.
        let watcher = self.clone();
        let watcher_state = state.clone();
        let instance_id = instance.id.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(400));
            let engine = match watcher.lock().as_ref() {
                Some(a) if a.id == id => a.engine.clone(),
                _ => return,
            };
            if let Some(error) = engine.take_error() {
                watcher.teardown_with_error(&watcher_state, error);
                return;
            }
            if engine.is_closed() {
                let had_replay = engine.replay_active();
                watcher.teardown(&watcher_state, "game_closed");
                if had_replay {
                    // Minecraft may have replaced its window; follow it.
                    let state = Arc::new(watcher_state.clone());
                    let id = instance_id.clone();
                    crate::runtime::spawn(async move { start_replay_when_ready(state, id).await });
                }
                return;
            }
        });
        Ok(engine)
    }

    pub fn start_recording(
        &self,
        state: &AppState,
        instance: &MinecraftInstance,
        pid: u32,
        settings: &RecordingSettings,
    ) -> AppResult<RecorderStatus> {
        if self.status().recording {
            return Err(AppError::new("Already recording.", "recorder: start while recording"));
        }
        let engine = self.ensure_engine(state, instance, pid, settings, settings.replay_enabled)?;
        let dir = recordings_dir(instance);
        std::fs::create_dir_all(&dir)?;
        engine.start_recording(unique_path(&dir, &timestamp()));
        if let Some(a) = self.lock().as_mut() {
            a.recording_started_at = Some(now_secs());
        }
        self.emit_status(state);
        Ok(self.status())
    }

    /// Stops the recording and saves it. Keeps the replay buffer running.
    pub fn stop_recording(&self, state: &AppState) -> Option<RecordingSaved> {
        let (engine, instance_id) = self.engine()?;
        if !engine.is_recording() {
            return None;
        }
        let result = engine.stop_recording();
        if let Some(a) = self.lock().as_mut() {
            a.recording_started_at = None;
        }
        if !engine.replay_active() {
            self.teardown(state, "stopped");
        }
        let saved = saved_event(instance_id, "recording", "stopped", result);
        self.emit_status(state);
        let _ = state.events.emit(
            Event::raw(EventName::RecorderSaved, saved.clone()).expect("RecordingSaved serializes"),
        );
        Some(saved)
    }

    pub fn save_replay(&self, state: &AppState) -> RecordingSaved {
        let Some((engine, instance_id)) = self.engine().filter(|(e, _)| e.replay_active()) else {
            let saved = RecordingSaved {
                instance_id: String::new(),
                files: Vec::new(),
                kind: "replay",
                reason: "failed",
                error: Some("Instant replay isn't running. Turn it on in Settings › Recording.".into()),
            };
            let _ = state.events.emit(
                Event::raw(EventName::RecorderSaved, saved.clone()).expect("RecordingSaved serializes"),
            );
            return saved;
        };
        let result = (|| {
            let instance = crate::runtime::block_on(state.instances.get(&instance_id)).map_err(|e| e.message)?;
            let dir = recordings_dir(&instance);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            engine.save_replay(&unique_path(&dir, &format!("Replay_{}", timestamp()))).map(Some)
        })();
        let saved = saved_event(instance_id, "replay", "saved", result);
        let _ = state.events.emit(
            Event::raw(EventName::RecorderSaved, saved.clone()).expect("RecordingSaved serializes"),
        );
        saved
    }

    fn teardown_with_error(&self, state: &AppState, error: String) {
        let Some(active) = self.lock().take() else { return };
        let result = active.engine.shutdown();
        self.emit_status(state);
        let files = result.and_then(|r| r.ok()).flatten();
        let _ = state.events.emit(
            Event::raw(
                EventName::RecorderSaved,
                RecordingSaved {
                    instance_id: active.instance_id,
                    files: files.iter().filter_map(|p| file_info(p)).collect(),
                    kind: "recording",
                    reason: "failed",
                    error: Some(format!("Recording stopped: {error}")),
                },
            )
            .expect("RecordingSaved serializes"),
        );
    }

    /// Stops capturing entirely, saving a recording in progress.
    pub fn teardown(&self, state: &AppState, reason: &'static str) {
        let Some(active) = self.lock().take() else { return };
        let was_recording = active.recording_started_at.is_some();
        let result = active.engine.shutdown();
        self.emit_status(state);
        if was_recording {
            let result = result.unwrap_or(Ok(None));
            let _ = state.events.emit(
                Event::raw(EventName::RecorderSaved, saved_event(active.instance_id, "recording", reason, result))
                    .expect("RecordingSaved serializes"),
            );
        }
    }

    pub fn current_instance(&self) -> Option<String> {
        self.lock().as_ref().map(|a| a.instance_id.clone())
    }
}

fn saved_event(
    instance_id: String,
    kind: &'static str,
    reason: &'static str,
    result: Result<Option<PathBuf>, String>,
) -> RecordingSaved {
    match result {
        Ok(path) => RecordingSaved {
            instance_id,
            files: path.iter().filter_map(|p| file_info(p)).collect(),
            kind,
            reason,
            error: None,
        },
        Err(e) => {
            log::error!("recorder: {e}");
            RecordingSaved { instance_id, files: Vec::new(), kind, reason: "failed", error: Some(e) }
        }
    }
}

/// Starts the replay buffer for a running game once its window exists
/// (Minecraft takes a while to open it). Gives up when the game exits, the
/// setting is turned off, or a newer request replaces this one.
pub async fn start_replay_when_ready(state: Arc<AppState>, instance_id: String) {
    let recorder = state.recorder.clone();
    let generation = recorder.replay_generation.fetch_add(1, Ordering::SeqCst) + 1;
    for _ in 0..300 {
        if recorder.replay_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let settings = state.settings.get().await.recording;
        if !settings.replay_enabled || !is_supported() {
            return;
        }
        let Some(pid) = state.running.get(&instance_id).await.and_then(|r| r.pid) else { return };
        let Ok(instance) = state.instances.get(&instance_id).await else { return };
        if recorder.current_instance().is_some() {
            return;
        }
        #[cfg(windows)]
        let ready = win::engine::find_game_window(pid).is_some();
        #[cfg(not(windows))]
        let ready = false;
        if ready {
            // Give the window a moment to settle at its real size.
            tokio::time::sleep(Duration::from_secs(2)).await;
            let r = recorder.clone();
            let s = state.clone();
            let result = tokio::task::spawn_blocking(move || r.ensure_engine(&s, &instance, pid, &settings, true)).await;
            match result {
                Ok(Ok(_)) => recorder.emit_status(&state),
                Ok(Err(e)) => log::error!("recorder: instant replay didn't start: {}", e.technical),
                Err(e) => log::error!("recorder: instant replay task: {e}"),
            }
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Applies changed settings to a running capture: restarts the replay
/// buffer with the new options (never interrupting a recording).
pub async fn apply_settings(state: Arc<AppState>) {
    let recorder = state.recorder.clone();
    if recorder.status().recording {
        return;
    }
    let state_for_teardown = state.clone();
    let r = recorder.clone();
    let _ = tokio::task::spawn_blocking(move || r.teardown(&state_for_teardown, "stopped")).await;
    if state.settings.get().await.recording.replay_enabled {
        if let Some(latest) = state.running.list().await.into_iter().max_by_key(|r| r.started_at) {
            crate::runtime::spawn(start_replay_when_ready(state.clone(), latest.instance_id));
        }
    }
}

async fn toggle_recording_from_hotkey(state: Arc<AppState>) {
    let recorder = state.recorder.clone();
    if recorder.status().recording {
        let s = state.clone();
        let _ = tokio::task::spawn_blocking(move || recorder.stop_recording(&s)).await;
        return;
    }
    let Some(target) = state.running.list().await.into_iter().max_by_key(|r| r.started_at) else { return };
    let (Some(pid), Ok(instance)) = (target.pid, state.instances.get(&target.instance_id).await) else { return };
    let settings = state.settings.get().await.recording;
    let s = state.clone();
    let result =
        tokio::task::spawn_blocking(move || recorder.start_recording(&s, &instance, pid, &settings)).await;
    if let Ok(Err(e)) = result {
        let _ = state.events.emit(
            Event::raw(
                EventName::RecorderSaved,
                RecordingSaved {
                    instance_id: target.instance_id,
                    files: Vec::new(),
                    kind: "recording",
                    reason: "failed",
                    error: Some(e.message),
                },
            )
            .expect("RecordingSaved serializes"),
        );
    }
}

async fn save_replay_from_hotkey(state: Arc<AppState>) {
    let recorder = state.recorder.clone();
    let _ = tokio::task::spawn_blocking(move || recorder.save_replay(&state)).await;
}

/// Registers the recording shortcuts while a game is running (and the
/// features are on), and releases them otherwise — so they never swallow
/// those key combinations from other apps when Minecraft isn't open.
pub async fn sync_hotkey(state: Arc<AppState>) {
    let settings = state.settings.get().await.recording;
    let game_running = !state.running.list().await.is_empty();
    let mut wanted: Vec<(String, bool)> = Vec::new();
    if is_supported() && game_running {
        if settings.hotkey_enabled {
            wanted.push((settings.hotkey.clone(), false));
        }
        if settings.replay_enabled && settings.replay_hotkey != settings.hotkey {
            wanted.push((settings.replay_hotkey.clone(), true));
        }
    }

    let recorder = &state.recorder;
    let mut registered = recorder.hotkeys.lock().unwrap_or_else(|e| e.into_inner());
    let wanted_keys: Vec<String> = wanted.iter().map(|(k, _)| k.clone()).collect();
    if *registered == wanted_keys {
        return;
    }
    for old in registered.drain(..) {
        if let Err(e) = state.hotkeys.unregister(&old) {
            log::error!("recorder: unregistering {old}: {e}");
        }
    }
    for (hotkey, is_replay) in wanted {
        let callback_state = state.clone();
        let callback = move || {
            if is_replay {
                crate::runtime::spawn(save_replay_from_hotkey(callback_state.clone()));
            } else {
                crate::runtime::spawn(toggle_recording_from_hotkey(callback_state.clone()));
            }
        };
        let result = state.hotkeys.register(hotkey.as_str(), Box::new(callback));
        match result {
            Ok(()) => registered.push(hotkey),
            // Usually another app already owns this combination.
            Err(e) => log::error!("recorder: registering {hotkey}: {e}"),
        }
    }
}

/// Checks a shortcut string parses (same notation as
/// `tauri_plugin_global_shortcut::Shortcut`, e.g. "Ctrl+Alt+R"): an
/// optional list of modifiers joined with "+", then exactly one key.
pub fn validate_hotkey(hotkey: &str) -> AppResult<()> {
    parse_hotkey(hotkey).map(|_| ()).map_err(|e| {
        AppError::new(format!("\"{hotkey}\" isn't a valid shortcut."), e.to_string())
    })
}

/// The superset of shortcut syntax the settings panel's key capture can
/// produce, mirroring the plugin's parser: modifiers `Ctrl`/`Control`,
/// `Alt`, `Shift`, `Meta`/`Super`/`Command` and the key itself as
/// `F1`–`F24`, `Key<…>` or a single alphanumeric character.
fn parse_hotkey(hotkey: &str) -> Result<(), &'static str> {
    let mut saw_key = false;
    for part in hotkey.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return Err("empty component");
        }
        if saw_key {
            return Err("key must be the last component");
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "alt" | "shift" | "meta" | "super" | "command" => {}
            key => {
                if key.starts_with("key") && key.len() == 4 {
                    if !key[3..].chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
                        return Err("unknown key");
                    }
                    saw_key = true;
                    continue;
                }
                let is_function_key = key.starts_with('f')
                    && key[1..].parse::<u32>().ok().is_some_and(|n| (1..=24).contains(&n));
                if is_function_key {
                    saw_key = true;
                    continue;
                }
                if key.chars().count() == 1 && key.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
                    saw_key = true;
                    continue;
                }
                return Err("unknown key");
            }
        }
    }
    if !saw_key {
        return Err("missing key");
    }
    Ok(())
}

pub fn list_microphones() -> Vec<Microphone> {
    #[cfg(windows)]
    {
        win::audio::list_microphones()
            .into_iter()
            .map(|m| Microphone { id: m.id, name: m.name, is_default: m.is_default })
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub async fn list_recordings(instance: &MinecraftInstance) -> AppResult<Vec<RecordingInfo>> {
    let dir = recordings_dir(instance);
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut list: Vec<RecordingInfo> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mp4")))
            .filter_map(|p| file_info(&p))
            .collect();
        list.sort_by_key(|r| std::cmp::Reverse(r.recorded_at));
        list
    })
    .await
    .map_err(|e| AppError::new("Couldn't read this instance's recordings.", e.to_string()))
}

fn recording_path(instance: &MinecraftInstance, filename: &str) -> AppResult<PathBuf> {
    let path = safe_join(&recordings_dir(instance), filename)
        .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
    if !path.is_file() {
        return Err(AppError::not_found("That recording"));
    }
    Ok(path)
}

pub async fn delete_recording(instance: &MinecraftInstance, filename: &str) -> AppResult<()> {
    let path = safe_join(&recordings_dir(instance), filename)
        .ok_or_else(|| AppError::new("Invalid file name.", "path traversal blocked"))?;
    // The launcher's own video player may still be letting go of the file.
    for attempt in 0.. {
        match tokio::fs::remove_file(&path).await {
            Err(e) if attempt < 4 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            result => return result.map_err(Into::into),
        }
    }
    unreachable!()
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "clip".into())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn seconds_to_ticks(seconds: f64) -> i64 {
    (seconds.max(0.0) * 10_000_000.0) as i64
}

/// Saves `[start, end)` seconds of a recording as a new file, without
/// re-encoding (the start snaps to the next keyframe, under a second).
pub async fn trim_recording(instance: &MinecraftInstance, filename: &str, start: f64, end: f64) -> AppResult<RecordingInfo> {
    if !is_supported() {
        return Err(unsupported());
    }
    if end - start < 0.5 {
        return Err(AppError::new("Pick at least half a second to keep.", "recorder: trim too short"));
    }
    let input = recording_path(instance, filename)?;
    let out = unique_path(&recordings_dir(instance), &format!("{}_trim", stem(&input)));
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let inputs = [win::remux::Input { path: input, offset: 0 }];
            win::remux::join(&inputs, seconds_to_ticks(start), Some(seconds_to_ticks(end)), &out)
                .map_err(|e| AppError::new("Couldn't trim this recording.", e.to_string()))?;
        }
        #[cfg(not(windows))]
        let _ = (input, start, end);
        file_info(&out).ok_or_else(|| AppError::new("Couldn't trim this recording.", "trim output missing"))
    })
    .await
    .map_err(|e| AppError::new("Couldn't trim this recording.", e.to_string()))?
}

/// Re-encodes `[start, end)` seconds into a file under `target_mb` (720p),
/// e.g. for Discord's upload limit.
pub async fn compress_recording(
    instance: &MinecraftInstance,
    filename: &str,
    start: f64,
    end: f64,
    target_mb: u32,
) -> AppResult<RecordingInfo> {
    if !is_supported() {
        return Err(unsupported());
    }
    let input = recording_path(instance, filename)?;
    let out = unique_path(&recordings_dir(instance), &format!("{}_{target_mb}MB", stem(&input)));
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let target = u64::from(target_mb.clamp(1, 500)) * 1024 * 1024;
            win::remux::compress(&input, seconds_to_ticks(start), seconds_to_ticks(end), target, 720, &out)
                .map_err(|e| AppError::new(e.clone(), format!("compress failed: {e}")))?;
        }
        #[cfg(not(windows))]
        let _ = (input, start, end, target_mb);
        file_info(&out).ok_or_else(|| AppError::new("Couldn't make a smaller copy.", "compress output missing"))
    })
    .await
    .map_err(|e| AppError::new("Couldn't make a smaller copy.", e.to_string()))?
}

/// Puts the file on the clipboard, ready to paste into Discord.
pub async fn copy_recording(instance: &MinecraftInstance, filename: &str) -> AppResult<()> {
    let path = recording_path(instance, filename)?;
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            win::remux::copy_file_to_clipboard(&path).map_err(|e| AppError::new("Couldn't copy it to the clipboard.", e))
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            Err(unsupported())
        }
    })
    .await
    .map_err(|e| AppError::new("Couldn't copy it to the clipboard.", e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape the settings panel's key capture can produce.
    #[test]
    fn shortcut_capture_output_parses() {
        for ok in [
            RecordingSettings::DEFAULT_HOTKEY,
            RecordingSettings::DEFAULT_REPLAY_HOTKEY,
            "F9",
            "Alt+F9",
            "Ctrl+Shift+1",
            "F24",
        ] {
            assert!(validate_hotkey(ok).is_ok(), "{ok}");
        }
        assert!(validate_hotkey("Ctrl+").is_err());
        assert!(validate_hotkey("Banana").is_err());
    }

    #[test]
    fn unique_path_counts_up() {
        let dir = std::env::temp_dir().join(format!("arcane-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a"), dir.join("a.mp4"));
        std::fs::write(dir.join("a.mp4"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a"), dir.join("a (2).mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
