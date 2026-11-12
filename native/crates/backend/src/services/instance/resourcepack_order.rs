use std::path::{Path, PathBuf};

use crate::error::AppResult;
use crate::models::instance::MinecraftInstance;

const FILE_PREFIX: &str = "file/";

fn options_txt_path(instance: &MinecraftInstance) -> PathBuf {
    Path::new(&instance.game_dir).join("options.txt")
}

/// Minecraft's own `resourcePacks:[...]` array in options.txt, parsed with
/// the same ordering it uses: the FIRST entry has the LOWEST priority and
/// the LAST entry has the HIGHEST (shown at the top of the in-game list,
/// and wins any asset conflicts). Entries are either a pseudo-pack name
/// (`"vanilla"`, `"fabric"`, ...) or `"file/<name>"` for an actual pack file.
async fn read_raw_order(instance: &MinecraftInstance) -> Vec<String> {
    let Ok(text) = tokio::fs::read_to_string(options_txt_path(instance)).await else {
        return vec!["vanilla".to_string()];
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("resourcePacks:") {
            return parse_array(rest);
        }
    }
    vec!["vanilla".to_string()]
}

fn parse_array(raw: &str) -> Vec<String> {
    let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
    trimmed
        .split(',')
        .map(|s| s.trim().trim_matches('"').replace("\\\"", "\""))
        .filter(|s| !s.is_empty())
        .collect()
}

fn serialize_array(entries: &[String]) -> String {
    let quoted: Vec<String> = entries
        .iter()
        .map(|e| format!("\"{}\"", e.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

/// Overwrites just the `resourcePacks` line, preserving every other
/// options.txt line untouched (dozens of unrelated settings live in that
/// file). Appends the line if it doesn't exist yet.
async fn write_raw_order(instance: &MinecraftInstance, entries: &[String]) -> AppResult<()> {
    let path = options_txt_path(instance);
    let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let new_line = format!("resourcePacks:{}", serialize_array(entries));

    let mut found = false;
    let mut lines: Vec<String> = existing
        .lines()
        .map(|line| {
            if line.starts_with("resourcePacks:") {
                found = true;
                new_line.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    if !found {
        lines.push(new_line);
    }
    let mut output = lines.join("\n");
    output.push('\n');
    tokio::fs::write(&path, output).await?;
    Ok(())
}

/// The current resource-pack-file order, lowest to highest priority, as
/// plain filenames (the `file/` prefix stripped) — reconciled against what
/// is actually enabled on disk so a stale or externally-added entry never
/// shows a pack that isn't really there.
pub async fn get_order(instance: &MinecraftInstance, enabled_files: &[String]) -> Vec<String> {
    let raw = read_raw_order(instance).await;
    let mut ordered: Vec<String> = raw
        .iter()
        .filter_map(|e| e.strip_prefix(FILE_PREFIX))
        .map(|s| s.to_string())
        .filter(|f| enabled_files.contains(f))
        .collect();
    for file in enabled_files {
        if !ordered.contains(file) {
            ordered.insert(0, file.clone());
        }
    }
    ordered
}

/// Writes back a new order (lowest to highest priority, matching
/// options.txt's own convention exactly). Pseudo-entries already present
/// (`vanilla`, `fabric`, ...) are kept, always beneath every real pack file.
pub async fn set_order(instance: &MinecraftInstance, ordered_files: Vec<String>) -> AppResult<()> {
    let raw = read_raw_order(instance).await;
    let mut new_order: Vec<String> = raw
        .into_iter()
        .filter(|e| !e.starts_with(FILE_PREFIX))
        .collect();
    new_order.extend(
        ordered_files
            .into_iter()
            .map(|f| format!("{FILE_PREFIX}{f}")),
    );
    write_raw_order(instance, &new_order).await
}

/// Removes a pack from the active order — called when a resource pack is
/// disabled, since Minecraft otherwise warns about a "missing" pack still
/// listed in options.txt.
pub async fn remove_entry(instance: &MinecraftInstance, file_name: &str) -> AppResult<()> {
    let raw = read_raw_order(instance).await;
    let target = format!("{FILE_PREFIX}{file_name}");
    let filtered: Vec<String> = raw.into_iter().filter(|e| e != &target).collect();
    write_raw_order(instance, &filtered).await
}

/// Adds a pack to the active order (lowest priority, i.e. the front of the
/// file-entries) if it isn't already present — called when a resource pack
/// is installed or re-enabled.
pub async fn ensure_entry_present(instance: &MinecraftInstance, file_name: &str) -> AppResult<()> {
    let mut raw = read_raw_order(instance).await;
    let target = format!("{FILE_PREFIX}{file_name}");
    if raw.iter().any(|e| e == &target) {
        return Ok(());
    }
    let insert_at = raw
        .iter()
        .position(|e| e.starts_with(FILE_PREFIX))
        .unwrap_or(raw.len());
    raw.insert(insert_at, target);
    write_raw_order(instance, &raw).await
}
