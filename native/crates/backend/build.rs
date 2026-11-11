use std::{env, fs, path::{Path, PathBuf}};

/// Bakes a build-time value into the binary as `env!(<out>)` so it never
/// lives in source control. Taken from the `<env_var>` environment variable,
/// else the gitignored key file. Neither present is fine — the value is then
/// empty and the feature degrades gracefully (see each consumer).
///
/// Difference from the Tauri build (src-tauri/build.rs): the key files stay
/// in their original location (`src-tauri/*.key`, gitignored) so both the
/// Tauri and native builds read the one true copy during migration. See
/// MIGRATION.md §2.1.
fn bake(manifest_dir: &Path, env_var: &str, file: &str, out: &str) {
    let key_path = manifest_dir.join("../../src-tauri").join(file);
    println!("cargo:rerun-if-env-changed={env_var}");
    println!("cargo:rerun-if-changed={}", key_path.display());
    let value = env::var(env_var)
        .ok()
        .or_else(|| fs::read_to_string(&key_path).ok())
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env={out}={value}");
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    // MineSkin API key — anonymous (much lower rate limits) if absent.
    bake(&manifest_dir, "MINESKIN_API_KEY", "mineskin.key", "ARCANE_MINESKIN_API_KEY");
    // Arcane's Azure app registration — Microsoft sign-in is unavailable in
    // a build that has none.
    bake(&manifest_dir, "MICROSOFT_CLIENT_ID", "microsoft-client-id.key", "ARCANE_MICROSOFT_CLIENT_ID");
    // Arcane's Discord application ID — Rich Presence is skipped without it.
    bake(&manifest_dir, "DISCORD_CLIENT_ID", "discord-client-id.key", "ARCANE_DISCORD_CLIENT_ID");
}
