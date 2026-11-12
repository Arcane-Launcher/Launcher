//! Build-time font fetcher.
//!
//! The launcher UI uses six self-hosted families (see `src/app/layout.tsx`
//! in the Tauri app — Sora, Poppins, Inter, Manrope, Outfit, JetBrains Mono,
//! weights 400/500/600/700). GPUI/DirectWrite needs raw TTF data, which the
//! @fontsource packages don't ship, so the TTFs are pulled from Google Fonts
//! the same way the Tauri build resolves them.
//!
//! The files live in `assets/fonts/` which is **gitignored**: they are never
//! committed to the repository (keeps the GitHub push small — see
//! MIGRATION.md §8) and are embedded into the binary via `include_bytes!`
//! in `src/theme.rs`, so the shipped app needs no network access.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// (file stem, Google Fonts family slug)
const FAMILIES: &[(&str, &str)] = &[
    ("sora", "Sora"),
    ("poppins", "Poppins"),
    ("inter", "Inter"),
    ("manrope", "Manrope"),
    ("outfit", "Outfit"),
    ("jetbrains-mono", "JetBrains+Mono"),
];
const WEIGHTS: &[u32] = &[400, 500, 600, 700];

/// Safari 4 user agent: Google Fonts serves per-weight static raw TTFs to
/// it (`00 01 00 00` sfnt). Modern UAs get WOFF2 (which DirectWrite's
/// factory-1 loader can't instance), and IE-era UAs get EOT.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; U; Intel Mac OS X 10_6_4; en-us) AppleWebKit/533.16 (KHTML, like Gecko) Version/4.1 Safari/533.16";

fn curl(args: &[&str]) -> Vec<u8> {
    // `--retry` covers the transient runner network blips that broke CI
    // once (a font fetch returned an empty body); `--retry-all-errors`
    // retries even non-5xx failures, and `-sS` keeps error messages
    // visible (plain `-s` hides them, which made a past failure
    // undiagnosable).
    let output = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "60",
            "--retry",
            "3",
            "--retry-delay",
            "2",
            "--retry-all-errors",
            "-A",
            USER_AGENT,
        ])
        .args(args)
        .output()
        .expect("failed to spawn curl (needed to fetch UI fonts for the build)");
    if !output.status.success() {
        panic!(
            "curl {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output.stdout
}

fn fetch_font(family: &str, weight: u32) -> Vec<u8> {
    let url = format!("https://fonts.googleapis.com/css2?family={family}:wght@{weight}");
    let css = String::from_utf8_lossy(&curl(&[&url])).into_owned();

    // Cheap parse, no regex dependency: `src: url(<URL>);`
    let start = css
        .find("url(")
        .expect("google fonts css response did not contain a font URL")
        + 4;
    let end = css[start..]
        .find(')')
        .expect("unterminated url() in google fonts css response");
    let url = &css[start..start + end];

    curl(&[url, "-o", "-"])
}

fn main() {
    let fonts_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("fonts");
    std::fs::create_dir_all(&fonts_dir).expect("failed to create assets/fonts directory");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/fonts");

    for (stem, family) in FAMILIES {
        for weight in WEIGHTS {
            let path = fonts_dir.join(format!("{stem}-{weight}.ttf"));
            // A stale/partial download (or an HTML error page) is replaced.
            let valid = std::fs::read(&path)
                .ok()
                .filter(|data| data.len() > 10_000 && is_ttf(data));
            if valid.is_some() {
                continue;
            }
            print!("fetching {stem} {weight}... ");
            std::io::stdout().flush().ok();
            let data = fetch_font(family, *weight);
            if data.len() <= 10_000 || !is_ttf(&data) {
                panic!(
                    "downloaded {stem}-{weight}.ttf does not look like a TTF ({} bytes) — delete assets/fonts and rebuild",
                    data.len()
                );
            }
            std::fs::write(&path, &data).expect("failed to write fetched font file");
            println!("ok ({} bytes)", data.len());
        }
    }
}

/// TTF magic: `00 01 00 00` (TrueType) or `OTTO` (CFF).
fn is_ttf(data: &[u8]) -> bool {
    data.len() >= 4 && (data[0..4] == [0x00, 0x01, 0x00, 0x00] || &data[0..4] == b"OTTO")
}
