use std::sync::Arc;

use crate::error::AppError;
use crate::models::settings::{LauncherSettings, RecordingSettings};
use crate::services::recorder::{self, Microphone, RecorderStatus, RecordingInfo, RecordingSaved};
use crate::state::AppState;

fn task_error(e: impl std::fmt::Display) -> AppError {
    AppError::new("Something went wrong with the recorder.", e.to_string())
}

pub fn recorder_status(state: &AppState) -> RecorderStatus {
    state.recorder.status()
}

pub async fn start_recording(
    state: &AppState,
    instance_id: String,
) -> Result<RecorderStatus, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    let pid = state
        .running
        .get(&instance_id)
        .await
        .and_then(|r| r.pid)
        .ok_or_else(|| AppError::new("Start the game first, then record.", "recorder: instance not running"))?;
    let settings = state.settings.get().await.recording;
    let recorder = state.recorder.clone();
    let state = state.clone();
    tokio::task::spawn_blocking(move || recorder.start_recording(&state, &instance, pid, &settings))
        .await
        .map_err(task_error)?
}

pub async fn stop_recording(state: &AppState) -> Result<Option<RecordingSaved>, AppError> {
    let recorder = state.recorder.clone();
    let state = state.clone();
    tokio::task::spawn_blocking(move || recorder.stop_recording(&state))
        .await
        .map_err(task_error)
}

pub async fn save_replay(state: &AppState) -> Result<RecordingSaved, AppError> {
    let recorder = state.recorder.clone();
    let state = state.clone();
    tokio::task::spawn_blocking(move || recorder.save_replay(&state))
        .await
        .map_err(task_error)
}

pub async fn list_microphones() -> Result<Vec<Microphone>, AppError> {
    tokio::task::spawn_blocking(recorder::list_microphones).await.map_err(task_error)
}

pub async fn list_recordings(state: &AppState, instance_id: String) -> Result<Vec<RecordingInfo>, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    recorder::list_recordings(&instance).await
}

pub async fn delete_recording(
    state: &AppState,
    instance_id: String,
    filename: String,
) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    recorder::delete_recording(&instance, &filename).await
}

pub async fn trim_recording(
    state: &AppState,
    instance_id: String,
    filename: String,
    start: f64,
    end: f64,
) -> Result<RecordingInfo, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    recorder::trim_recording(&instance, &filename, start, end).await
}

pub async fn compress_recording(
    state: &AppState,
    instance_id: String,
    filename: String,
    start: f64,
    end: f64,
    target_mb: u32,
) -> Result<RecordingInfo, AppError> {
    let instance = state.instances.get(&instance_id).await?;
    recorder::compress_recording(&instance, &filename, start, end, target_mb).await
}

pub async fn copy_recording(state: &AppState, instance_id: String, filename: String) -> Result<(), AppError> {
    let instance = state.instances.get(&instance_id).await?;
    recorder::copy_recording(&instance, &filename).await
}

pub async fn set_recording_settings(
    state: &AppState,
    recording: RecordingSettings,
) -> Result<LauncherSettings, AppError> {
    let recording = recording.sanitized();
    recorder::validate_hotkey(&recording.hotkey)?;
    recorder::validate_hotkey(&recording.replay_hotkey)?;
    if recording.hotkey == recording.replay_hotkey {
        return Err(AppError::new(
            "Recording and instant replay need different shortcuts.",
            "recorder: duplicate hotkeys",
        ));
    }
    let before = state.settings.get().await.recording;
    let updated = state.settings.update(|s| s.recording = recording.clone()).await?;
    recorder::sync_hotkey(Arc::new(state.clone())).await;
    // Hotkey-only changes don't need the capture restarted.
    let capture_changed = RecordingSettings { hotkey: String::new(), hotkey_enabled: false, replay_hotkey: String::new(), ..before }
        != RecordingSettings { hotkey: String::new(), hotkey_enabled: false, replay_hotkey: String::new(), ..recording };
    if capture_changed {
        recorder::apply_settings(Arc::new(state.clone())).await;
    }
    Ok(updated)
}
