//! Reads the mod ids a jar declares (Fabric, Quilt, Forge, NeoForge), so a
//! crash log that names a mod by id ("sodium") can be traced back to the
//! file in `mods/` it came from.

use std::io::Read;
use std::path::Path;

use crate::models::instance::MinecraftInstance;

use super::mods::mods_dir;

#[derive(Debug, Clone)]
pub struct ModJar {
    pub file_name: String,
    /// Declared ids, lowercase. Empty for a jar with no loader metadata
    /// (OptiFine, a corrupted download, ...).
    pub ids: Vec<String>,
}

/// Every enabled `.jar` in the instance's `mods/` folder with the ids it
/// declares. Unreadable jars are still listed, with no ids.
pub async fn scan_mod_jars(instance: &MinecraftInstance) -> Vec<ModJar> {
    let dir = mods_dir(instance);
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut jars: Vec<ModJar> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                let file_name = e.file_name().to_string_lossy().into_owned();
                file_name
                    .to_ascii_lowercase()
                    .ends_with(".jar")
                    .then(|| ModJar {
                        ids: read_mod_ids(&e.path()),
                        file_name,
                    })
            })
            .collect();
        jars.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        jars
    })
    .await
    .unwrap_or_default()
}

/// The jar in `jars` that declares `id` (compared case-insensitively, with
/// `-` and `_` treated alike since mods spell them both ways).
pub fn find_jar<'a>(jars: &'a [ModJar], id: &str) -> Option<&'a ModJar> {
    let want = normalize_id(id);
    jars.iter()
        .find(|j| j.ids.iter().any(|i| normalize_id(i) == want))
}

pub fn normalize_id(id: &str) -> String {
    id.trim().to_ascii_lowercase().replace('_', "-")
}

fn read_mod_ids(path: &Path) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    let mut ids = Vec::new();

    if let Some(json) = read_entry(&mut zip, "fabric.mod.json") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                ids.push(id.to_ascii_lowercase());
            }
            if let Some(provides) = v.get("provides").and_then(|p| p.as_array()) {
                ids.extend(
                    provides
                        .iter()
                        .filter_map(|p| p.as_str())
                        .map(str::to_ascii_lowercase),
                );
            }
        }
    }
    if let Some(json) = read_entry(&mut zip, "quilt.mod.json") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            if let Some(id) = v.pointer("/quilt_loader/id").and_then(|i| i.as_str()) {
                ids.push(id.to_ascii_lowercase());
            }
        }
    }
    for toml in ["META-INF/mods.toml", "META-INF/neoforge.mods.toml"] {
        if let Some(text) = read_entry(&mut zip, toml) {
            ids.extend(toml_mod_ids(&text));
        }
    }

    ids.sort();
    ids.dedup();
    ids
}

/// `modId="..."` lines from a mods.toml. Only the `[[mods]]` entries use
/// that key (dependencies use `modId` too, but inside `[[dependencies.*]]`
/// tables, which are skipped).
fn toml_mod_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut in_mods = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("[[") {
            in_mods = line.starts_with("[[mods]]");
            continue;
        }
        if !in_mods {
            continue;
        }
        let Some(rest) = line.strip_prefix("modId") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if !value.is_empty() {
            ids.push(value.to_ascii_lowercase());
        }
    }
    ids
}

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    // Metadata files are a few KB; anything huge isn't one.
    if entry.size() > 1024 * 1024 {
        return None;
    }
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_mod_entries_from_mods_toml() {
        let toml = r#"
modLoader="javafml"
[[mods]]
modId="create"
version="0.5"
[[dependencies.create]]
    modId="forge"
    mandatory=true
[[mods]]
modId = 'flywheel'
"#;
        assert_eq!(toml_mod_ids(toml), vec!["create", "flywheel"]);
    }

    #[test]
    fn matches_ids_across_dash_and_underscore() {
        let jars = vec![ModJar {
            file_name: "dynamic-fps.jar".into(),
            ids: vec!["dynamic_fps".into()],
        }];
        assert_eq!(
            find_jar(&jars, "dynamic-fps").map(|j| j.file_name.as_str()),
            Some("dynamic-fps.jar")
        );
    }
}
