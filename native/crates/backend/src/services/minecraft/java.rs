use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DetectedJava {
    pub path: String,
    pub version: String,
}

fn candidate_names() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        vec!["javaw.exe", "java.exe"]
    } else {
        vec!["java"]
    }
}

/// Best-effort local Java detection: checks PATH and a handful of common
/// install locations. Phase 1 does not download/manage its own Java
/// runtimes, so this is what backs "point me at a JVM" in Settings.
pub async fn detect_java_installations() -> Vec<DetectedJava> {
    let mut found = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();

    for name in candidate_names() {
        if let Ok(path) = which(name) {
            candidates.push(path);
        }
    }

    if cfg!(target_os = "windows") {
        for base in [
            "C:/Program Files/Java",
            "C:/Program Files/Eclipse Adoptium",
            "C:/Program Files/Microsoft",
        ] {
            if let Ok(entries) = std::fs::read_dir(base) {
                for entry in entries.flatten() {
                    let bin = entry.path().join("bin").join("javaw.exe");
                    if bin.exists() {
                        candidates.push(bin);
                    }
                }
            }
        }
    }

    candidates.sort();
    candidates.dedup();

    for path in candidates {
        if let Some(version) = probe_version(&path).await {
            found.push(DetectedJava {
                path: path.to_string_lossy().to_string(),
                version,
            });
        }
    }

    found
}

pub async fn probe_version(path: &std::path::Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(path);
    cmd.arg("-version");
    crate::services::process_ext::suppress_console_window(&mut cmd);
    let output = cmd.output().await.ok()?;
    let text = String::from_utf8_lossy(&output.stderr);
    let first_line = text.lines().next().unwrap_or("").to_string();
    if first_line.is_empty() {
        None
    } else {
        Some(first_line)
    }
}

/// Parses the major version out of a `java -version` first line, handling
/// both the modern scheme (`"21.0.1"` → 21) and the pre-Java-9 scheme
/// (`"1.8.0_351"` → 8).
pub fn parse_major_version(version_line: &str) -> Option<u32> {
    let start = version_line.find('"')? + 1;
    let end = version_line[start..].find('"')? + start;
    let version = &version_line[start..end];
    let mut parts = version.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Starts the Java at `path` and reads its major version — `None` if it
/// won't start or prints something unrecognizable.
pub async fn major_version_of(path: &str) -> Option<u32> {
    if path.trim().is_empty() {
        return None;
    }
    parse_major_version(&probe_version(std::path::Path::new(path)).await?)
}

pub async fn is_valid_java(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    probe_version(std::path::Path::new(path)).await.is_some()
}

fn which(name: &str) -> std::io::Result<PathBuf> {
    let path_var = std::env::var_os("PATH")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "PATH not set"))?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "not found",
    ))
}
