use std::path::Path;

use crate::error::{AppError, AppResult};

/// Recursively copies `src` into `dst` (creating `dst` and any
/// subdirectories as needed). Used for instance duplication — libraries and
/// assets are shared and untouched by this; only the per-instance content
/// (mods, config, saves, etc.) actually gets copied.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> AppResult<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            std::fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}

/// Copies an instance folder brought in from another launcher. Top-level
/// entries `skip_top_level` rejects aren't copied, and a file that can't be
/// read (typically locked because the other launcher or game is still
/// running, like a world's `session.lock`) is skipped with a warning instead
/// of failing the whole import. Returns how many files were skipped that way.
pub fn copy_import_dir(
    src: &Path,
    dst: &Path,
    skip_top_level: &dyn Fn(&str) -> bool,
) -> AppResult<usize> {
    fn copy(src: &Path, dst: &Path, skipped: &mut usize) -> AppResult<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)?.flatten() {
            let path = entry.path();
            let dest_path = dst.join(entry.file_name());
            if path.is_dir() {
                copy(&path, &dest_path, skipped)?;
            } else if let Err(e) = std::fs::copy(&path, &dest_path) {
                log::warn!("import: skipped {} ({e})", path.display());
                *skipped += 1;
            }
        }
        Ok(())
    }

    std::fs::create_dir_all(dst)?;
    let mut skipped = 0;
    for entry in std::fs::read_dir(src)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if skip_top_level(&name) {
            continue;
        }
        let path = entry.path();
        let dest_path = dst.join(&name);
        if path.is_dir() {
            copy(&path, &dest_path, &mut skipped)?;
        } else if let Err(e) = std::fs::copy(&path, &dest_path) {
            log::warn!("import: skipped {} ({e})", path.display());
            skipped += 1;
        }
    }
    Ok(skipped)
}

/// Total size in bytes of every file under `path`, recursively. Missing or
/// unreadable directories are treated as empty rather than erroring, since
/// callers use this for best-effort storage reporting.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Zips the contents of `source_dir` into `zip_path` (paths inside the
/// archive are relative to `source_dir`, using `/` separators regardless of
/// platform). Shared by world backups and instance export.
pub fn zip_directory(source_dir: &Path, zip_path: &Path) -> AppResult<()> {
    if let Some(parent) = zip_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(zip_path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_dir_to_zip(&mut writer, source_dir, source_dir, &options)?;
    writer
        .finish()
        .map_err(|e| AppError::new("Couldn't finish writing the archive.", e.to_string()))?;
    Ok(())
}

fn add_dir_to_zip(
    writer: &mut zip::ZipWriter<std::fs::File>,
    base: &Path,
    dir: &Path,
    options: &zip::write::SimpleFileOptions,
) -> AppResult<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            writer
                .add_directory(format!("{relative}/"), *options)
                .map_err(|e| AppError::new("Couldn't write the archive.", e.to_string()))?;
            add_dir_to_zip(writer, base, &path, options)?;
        } else {
            writer
                .start_file(relative, *options)
                .map_err(|e| AppError::new("Couldn't write the archive.", e.to_string()))?;
            let mut file = std::fs::File::open(&path)?;
            std::io::copy(&mut file, writer)?;
        }
    }
    Ok(())
}
