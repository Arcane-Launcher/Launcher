//! Phosphor-bold icon set — extracted verbatim from `src/lib/icon-data.json`
//! (the Iconify `ph` collection the Tauri app renders via `@iconify/react`).
//!
//! gpui rasterizes SVGs into monochrome sprites tinted by the element's
//! text color, which is exactly what `fill="currentColor"` icons do in the
//! browser, so each path here is wrapped in a minimal 256×256 SVG and
//! rendered with `svg()` + `.text_color()`.
//!
//! The lucide `Download` stroke icon used by the download tray in the top
//! bar is included the same way (24×24 viewBox, `stroke="currentColor"`).

use gpui::{prelude::*, px, Pixels, SharedString, Svg};

/// Full SVG markup per icon, served through the asset source as
/// `icons/<name>.svg`.
macro_rules! phosphor {
    ($($name:ident => $data:literal),* $(,)?) => {
        $(
            pub const $name: &str = concat!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 256 256\" fill=\"currentColor\">",
                $data,
                "</svg>"
            );
        )*
    };
}

phosphor! {
    // Title bar window controls (title-bar.tsx: minus 12px, square/corners-in
    // 11px, x 13px; the same x is the toast dismiss button at 13px).
    MINUS => "<path fill=\"currentColor\" d=\"M228 128a12 12 0 0 1-12 12H40a12 12 0 0 1 0-24h176a12 12 0 0 1 12 12\"/>",
    SQUARE => "<path fill=\"currentColor\" d=\"M208 28H48a20 20 0 0 0-20 20v160a20 20 0 0 0 20 20h160a20 20 0 0 0 20-20V48a20 20 0 0 0-20-20m-4 176H52V52h152Z\"/>",
    CORNERS_IN => "<path fill=\"currentColor\" d=\"M148 96V48a12 12 0 0 1 24 0v36h36a12 12 0 0 1 0 24h-48a12 12 0 0 1-12-12m-52 52H48a12 12 0 0 0 0 24h36v36a12 12 0 0 0 24 0v-48a12 12 0 0 0-12-12m112 0h-48a12 12 0 0 0-12 12v48a12 12 0 0 0 24 0v-36h36a12 12 0 0 0 0-24M96 36a12 12 0 0 0-12 12v36H48a12 12 0 0 0 0 24h48a12 12 0 0 0 12-12V48a12 12 0 0 0-12-12\"/>",
    X => "<path fill=\"currentColor\" d=\"M208.49 191.51a12 12 0 0 1-17 17L128 145l-63.51 63.49a12 12 0 0 1-17-17L111 128L47.51 64.49a12 12 0 0 1 17-17L128 111l63.51-63.52a12 12 0 0 1 17 17L145 128Z\"/>",

    // Sidebar navigation icons (sidebar.tsx, 20px).
    PLAY => "<path fill=\"currentColor\" d=\"M234.49 111.07L90.41 22.94A20 20 0 0 0 60 39.87v176.26a20 20 0 0 0 30.41 16.93l144.08-88.13a19.82 19.82 0 0 0 0-33.86M84 208.85V47.15L216.16 128Z\"/>",
    COMPASS => "<path fill=\"currentColor\" d=\"M128 20a108 108 0 1 0 108 108A108.12 108.12 0 0 0 128 20m0 192a84 84 0 1 1 84-84a84.09 84.09 0 0 1-84 84m35.27-135l-56 24a12 12 0 0 0-6.3 6.3l-24 56A12 12 0 0 0 92.73 179l56-24a12 12 0 0 0 6.3-6.3l24-56A12 12 0 0 0 163.27 77m-28.41 57.89l-24 10.29l10.29-24l24-10.29Z\"/>",
    STACK => "<path fill=\"currentColor\" d=\"M234.36 170a12 12 0 0 1-4.36 16.37l-96 56a12 12 0 0 1-12.1 0l-96-56a12 12 0 0 1 12.09-20.74l90 52.48L218 165.63a12 12 0 0 1 16.36 4.37M218 117.63l-90 52.48l-89.95-52.48A12 12 0 0 0 26 138.37l96 56a12 12 0 0 0 12.1 0l96-56a12 12 0 0 0-12.1-20.74M20 80a12 12 0 0 1 6-10.37l96-56a12.06 12.06 0 0 1 12.1 0l96 56a12 12 0 0 1 0 20.74l-96 56a12 12 0 0 1-12.1 0l-96-56A12 12 0 0 1 20 80m35.82 0L128 122.11L200.18 80L128 37.89Z\"/>",
    DOWNLOAD_SIMPLE => "<path fill=\"currentColor\" d=\"M228 144v64a12 12 0 0 1-12 12H40a12 12 0 0 1-12-12v-64a12 12 0 0 1 24 0v52h152v-52a12 12 0 0 1 24 0m-108.49 8.49a12 12 0 0 0 17 0l40-40a12 12 0 0 0-17-17L140 115V32a12 12 0 0 0-24 0v83L96.49 95.51a12 12 0 0 0-17 17Z\"/>",
    USER => "<path fill=\"currentColor\" d=\"M234.38 210a123.36 123.36 0 0 0-60.78-53.23a76 76 0 1 0-91.2 0A123.36 123.36 0 0 0 21.62 210a12 12 0 1 0 20.77 12c18.12-31.32 50.12-50 85.61-50s67.49 18.69 85.61 50a12 12 0 0 0 20.77-12M76 96a52 52 0 1 1 52 52a52.06 52.06 0 0 1-52-52\"/>",
    GEAR_SIX => "<path fill=\"currentColor\" d=\"M128 76a52 52 0 1 0 52 52a52.06 52.06 0 0 0-52-52m0 80a28 28 0 1 1 28-28a28 28 0 0 1-28 28m113.86-49.57a12 12 0 0 0-5.86-8.09l-27.79-15.85l-.11-31.31a12 12 0 0 0-4.25-9.12a116 116 0 0 0-38-21.41a12 12 0 0 0-9.68.89L128 37.27L99.83 21.53a12 12 0 0 0-9.7-.9a116.1 116.1 0 0 0-38 21.47a12 12 0 0 0-4.24 9.1l-.14 31.34L20 98.35a12 12 0 0 0-5.85 8.11a110.7 110.7 0 0 0 0 43.11a12 12 0 0 0 5.85 8.09l27.82 15.85l.11 31.31a12 12 0 0 0 4.25 9.12a116 116 0 0 0 38 21.41a12 12 0 0 0 9.68-.89L128 218.73l28.14 15.74a12 12 0 0 0 9.7.9a116.1 116.1 0 0 0 38-21.47a12 12 0 0 0 4.24-9.1l.14-31.34l27.81-15.81a12 12 0 0 0 5.85-8.11a110.7 110.7 0 0 0-.02-43.11m-22.63 33.18l-26.88 15.28a11.94 11.94 0 0 0-4.55 4.59c-.54 1-1.11 1.93-1.7 2.88a12 12 0 0 0-1.83 6.31l-.14 30.33a91.8 91.8 0 0 1-21.07 11.87l-27.15-15.19a12 12 0 0 0-5.86-1.53h-3.73a12.1 12.1 0 0 0-6.14 1.51L93 210.82A92.3 92.3 0 0 1 71.88 199l-.11-30.24a12 12 0 0 0-1.83-6.32c-.58-.94-1.16-1.91-1.7-2.88A11.9 11.9 0 0 0 63.7 155l-26.9-15.37a86.5 86.5 0 0 1 0-23.24l26.88-15.28a12 12 0 0 0 4.55-4.58c.54-1 1.11-1.94 1.7-2.89a12 12 0 0 0 1.83-6.31L71.87 57a91.8 91.8 0 0 1 21.07-11.83l27.15 15.19a11.9 11.9 0 0 0 6.15 1.52h3.44a12.1 12.1 0 0 0 6.14-1.51L163 45.18A92.3 92.3 0 0 1 184.12 57l.11 30.24a12 12 0 0 0 1.83 6.32c.58.94 1.16 1.91 1.7 2.88a11.9 11.9 0 0 0 4.54 4.56l26.9 15.33a86.5 86.5 0 0 1 .03 23.28\"/>",

    // Toast variant icons (toaster.tsx, 17px).
    CHECK_CIRCLE => "<path fill=\"currentColor\" d=\"M176.49 95.51a12 12 0 0 1 0 17l-56 56a12 12 0 0 1-17 0l-24-24a12 12 0 1 1 17-17L112 143l47.51-47.52a12 12 0 0 1 16.98.03M236 128A108 108 0 1 1 128 20a108.12 108.12 0 0 1 108 108m-24 0a84 84 0 1 0-84 84a84.09 84.09 0 0 0 84-84\"/>",
    WARNING_CIRCLE => "<path fill=\"currentColor\" d=\"M128 20a108 108 0 1 0 108 108A108.12 108.12 0 0 0 128 20m0 192a84 84 0 1 1 84-84a84.09 84.09 0 0 1-84 84m-12-80V80a12 12 0 0 1 24 0v52a12 12 0 0 1-24 0m28 40a16 16 0 1 1-16-16a16 16 0 0 1 16 16\"/>",

    // Account switcher rows + spinner (13px/14px).
    PLUS => "<path fill=\"currentColor\" d=\"M228 128a12 12 0 0 1-12 12h-76v76a12 12 0 0 1-24 0v-76H40a12 12 0 0 1 0-24h76V40a12 12 0 0 1 24 0v76h76a12 12 0 0 1 12 12\"/>",
    TRASH => "<path fill=\"currentColor\" d=\"M216 48h-36V36a28 28 0 0 0-28-28h-48a28 28 0 0 0-28 28v12H40a12 12 0 0 0 0 24h4v136a20 20 0 0 0 20 20h128a20 20 0 0 0 20-20V72h4a12 12 0 0 0 0-24M100 36a4 4 0 0 1 4-4h48a4 4 0 0 1 4 4v12h-56Zm88 168H68V72h120Zm-72-100v64a12 12 0 0 1-24 0v-64a12 12 0 0 1 24 0m48 0v64a12 12 0 0 1-24 0v-64a12 12 0 0 1 24 0\"/>",
    ARROW_CLOCKWISE => "<path fill=\"currentColor\" d=\"M244 56v48a12 12 0 0 1-12 12h-48a12 12 0 1 1 0-24h17.1l-19-17.38c-.13-.12-.26-.24-.38-.37A76 76 0 1 0 127 204h1a75.53 75.53 0 0 0 52.15-20.72a12 12 0 0 1 16.49 17.45A99.45 99.45 0 0 1 128 228h-1.37a100 100 0 1 1 71.88-170.94L220 76.72V56a12 12 0 0 1 24 0\"/>",
}

/// lucide `Download` (download-tray.tsx) — 24×24 stroke icon.
pub const LUCIDE_DOWNLOAD: &str = concat!(
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" ",
    "stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">",
    "<path d=\"M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4\"/><polyline points=\"7 10 12 15 17 10\"/>",
    "<line x1=\"12\" x2=\"12\" y1=\"3\" y2=\"15\"/></svg>"
);

/// Asset-source path prefix all icon files live under.
pub const ASSET_DIR: &str = "icons";

/// (asset file name, SVG markup) for every icon in this module.
pub fn all_icons() -> &'static [(&'static str, &'static str)] {
    &[
        ("minus.svg", MINUS),
        ("square.svg", SQUARE),
        ("corners-in.svg", CORNERS_IN),
        ("x.svg", X),
        ("play.svg", PLAY),
        ("compass.svg", COMPASS),
        ("stack.svg", STACK),
        ("download-simple.svg", DOWNLOAD_SIMPLE),
        ("user.svg", USER),
        ("gear-six.svg", GEAR_SIX),
        ("check-circle.svg", CHECK_CIRCLE),
        ("warning-circle.svg", WARNING_CIRCLE),
        ("plus.svg", PLUS),
        ("trash.svg", TRASH),
        ("arrow-clockwise.svg", ARROW_CLOCKWISE),
        ("lucide-download.svg", LUCIDE_DOWNLOAD),
    ]
}

/// Renders an icon at the given pixel size, tinted by the inherited (or
/// explicitly set) text color.
pub fn icon(asset: &'static str, size: Pixels) -> Svg {
    gpui::svg()
        .path(SharedString::from(format!("{ASSET_DIR}/{asset}")))
        .w(size)
        .h(size)
        .flex_none()
}

/// Convenience: sidebar/window-control icons at their pixel sizes as used
/// by the Tauri app.
pub fn icon_at(asset: &'static str, size: f32) -> Svg {
    icon(asset, px(size))
}
