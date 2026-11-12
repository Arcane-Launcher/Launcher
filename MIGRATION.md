# Arcane Launcher — Tauri → Native Rust + GPUI Migration Plan

> **Note:** the Phase-0 commit message refers to a `MIGURATION_PLAN.md`; the actual artifact is this file, `MIGRATION.md`.

**Status:** Phases 0–2 complete (backend port, shell/auth chrome, launch slice). Phase 3 in progress — Play + Instances screens ported; next: Mods / Downloads / create+import dialogs.
**Source inventory date:** 2026-11-08, against repo state (`main` @ `6e6b9db`)
**Hard rule:** the existing Tauri project (`src/`, `src-tauri/`, `package.json`, …) is **never modified or deleted**. The native app is built alongside it; both can be built/run until the migration is finished.

---

## 0. Ground truth (facts, verified)

- **GPUI:** `gpui = "0.2.2"` on crates.io (2025-10-22, Apache-2.0, edition 2024, from zed-industries/zed). Platform features: `windows-manifest` (Windows), `x11`, `wayland` (Linux) — all in `default` features. macOS is also supported (unused by us). Docs at https://gpui.rs; no docs.rs docs.
- **Current app:** Tauri 2.11.3 shell + Next.js 16/React 19 webview frontend (1280×800, min 960×640, `decorations: false`, custom title bar), Rust backend in `src-tauri` (~110 commands, 14 events).
- **Data dir:** `%APPDATA%\com.arcanelauncher.app` — the native app **must keep the same identifier and byte-identical data layout** (`settings.json`, `auth.json`, `instances.json`, `instances/…`, `meta/…`, `java/…`, `cache/…`, `skins/`, `background.*`, `downloads/cache`).
- **Updater:** Cloudflare R2 (endpoint `https://update.arcaneworld.lk/latest.json`, beta channel `beta/latest.json`, rollback `versions/<v>.json`), minisign-signed, Windows `passive` install mode.

---

## 1. What exists today (summary of the audit)

### Frontend (WebView) — from full component/UI scan
- **Screens (7 routes):** Home, Instances, Mods (browse/details/installed), Downloads, Skin Studio, Settings (7 categories). Login screen gates the shell.
- **Shell:** custom title bar (h-9, drag region, min/max/close), left sidebar (84 px, 6 nav items), top bar (breadcrumb, download tray, network indicator, RAM pill, account switcher), global overlays (toasts, banners, crash/recorder/discord listeners, halloween extras, particle engine, whats-new dialog).
- **Design system:** CSS-variable theming — **22 theme presets** (10 palettes + 12 signature themes with built-in effects), **17 accent colors**, **6 font families** (Sora, Poppins, Inter, Manrope, Outfit, JetBrains Mono, weights 400–700), glassmorphism (`backdrop-filter` blur) as the core visual language, radius/shadow tokens, thin styled scrollbars.
- **Animations:** page transitions (220 ms fade+slide), window enter/exit choreography (180 ms), CSS particle engine for 16+ effects (petals/snow/bats/embers/…), ghost trail, halloween SFX via Web Audio synthesis, `prefers-reduced-motion` guards.
- **State:** 4 zustand stores + react-query caching layer (~110 invoke commands, 13 event subscriptions via `tauri-client.ts`).
- **WebView-only tech** (migration risk): glassmorphism blur, HTML5 video (login bg, recordings), skinview3d WebGL skin viewer, canvas skin compositing, markdown→HTML readme, SVG particle glyphs + filters, Web Audio synth, drag-region hit testing, `convertFileSrc` asset loading, native open/save dialogs, clipboard image copy.

### Backend (Rust)

- **~110 thin `#[tauri::command]`s** → all logic lives in services. `AppError { message, technical, retryable }` error shape.
- **Pure Rust, reusable verbatim (≈95% of the domain logic):** all of `models/`; `instance/` (18 modules: manager, store, mods, install, update_check, validation, health, crash_help, evolution, servers, server_ping, worlds, screenshots, mod_profiles, resourcepack_order, jar_ids, mod_identify, collection_import, performance); `minecraft/` (manifest, assets, libraries, java, java_provision, running, session_store, skins, mineskin, crash, and `launch.rs::build_launch_plan`); `loaders/` (fabric, forge, neoforge, quilt, forgewrapper); `providers/` (modrinth, curseforge); `modpack/`; `cache/`; `fs_utils`, `system`, `gpu` (Win API only), `compatibility`, `import`, `storage`, `background`, `process_ext`; `recorder/win/*` (entire Windows Graphics Capture / Media Foundation / WASAPI engine).
- **Tauri-coupled, needs small adapters:** `state.rs` (AppHandle-based), `paths.rs` (one function), `download/manager.rs` (1 emit), `minecraft/launch.rs` (4 emits), `minecraft/auth.rs` (2 emit sites + the login **embedded webview window**), `discord_rpc.rs` (3 emits), `network/connectivity.rs` (1 emit), `recorder/mod.rs` (emits + global-shortcut plugin + state lookups), `join_link.rs` (deep-link plugin), `update_on_exit.rs`, `updater.rs` (**deep** — wraps `tauri_plugin_updater`; biggest rewrite), `commands/*` (become plain async fns over `Arc<AppState>`).
- **Dropped:** `webview_memory.rs` (WebView2-only trick), WebView2 args, `tauri-plugin-*`.
- **Tokens baked via build.rs:** MineSkin key, Microsoft client ID, Discord client ID (from `.key` files / env) — reuse the same mechanism.

### Backend event/programming contract (must be preserved 1:1)
Events (emit → UI): `auth://progress`, `launch://started|log|exit`, `install://stage`, `download://progress`, `updater://progress`, `network://status`, `recorder://state|saved`, `discord://join|join_request|join_request_resolved`.

---

## 2. Target architecture

### 2.1 Repository layout (new code, no Tauri paths touched)

```
native/                          ← new Cargo workspace (git-ignored nothing; sources tracked)
├── Cargo.toml                   [workspace] members: crates/*  resolver = "2"
├── crates/
│   ├── backend/             ← domain layer: verbatim copies of the PURE modules
│   │     src/
│   │       models/…  instance/…  minecraft/…  loaders/…  providers/…
│   │       download/manager.rs (de-tauri'd)  network/  cache/  fs_utils  gpu
│   │       recorder/win/  system  storage  background  compatibility  import
│   │       events.rs            ← EventSink trait + Event enum (typed payloads)
│   │       state.rs             ← AppState (no AppHandle; Arc-backed fields)
│   │       paths.rs             ← AppPaths::new(root: PathBuf)  (same layout)
│   └── frontend/          ← native app (bin "arcane-launcher")
│         src/main.rs            ← tokio runtime + gpui startup
│         src/shell/             ← GPUI: title bar, sidebar, top bar, routing
│         src/screens/           ← home, instances, mods, downloads, skin, settings, login
│         src/components/        ← GPUI widget library (glass panel, buttons, badges, …)
│         src/theme/             ← 22 themes × 17 accents × 6 fonts as runtime structs
│         src/effects/           ← particle engine (GPUI canvas-equivalent)
│         src/platform/          ← hotkeys, deep link, chooser dialogs, clipboard, shell open
│         src/updater/           ← native self-updater (replaces tauri_plugin_updater)
│         src/auth_login/        ← Microsoft login flow host
│         resources/             ← fonts (6 families, woff2/ttf), app icon, images
```

The build.rs token-baking approach (`MINESKIN_API_KEY`, `MICROSOFT_CLIENT_ID`, `DISCORD_CLIENT_ID`) is copied to `backend` or `frontend` unchanged.

### 2.2 Event plumbing (the one abstraction that replaces Tauri)

```rust
// backend/src/events.rs
#[async_trait]
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: Event);
}
// Event is a typed enum of exactly the 14 payload kinds listed in §1;
// Service code that today does `app.emit("launch://log", …)` becomes
// `sink.emit(Event::LaunchLog(event))`, serialized payload byte-identical.
```

GPUI side: `EventSink` implementation pushes into an `Arc<Mutex<VecDeque<Event>>>` + `std::sync::mpsc`/waker that drains onto the GPUI main thread and updates GPUI `Entity` stores — the UI re-renders exactly where the react-query/zustand subscribers did before. A small reactive "store" layer reproduces zustand/react-query semantics (caching TTLs per §1 §6.3 where visible).

`AppState::initialize(&paths)` replaces `AppState::initialize(&app)`; every command wrapper in `commands/*` becomes `pub async fn <cmd>(state: &Arc<AppState>, …) -> Result<T, AppError>` — the ~110 command signatures are preserved so UI↔backend parity is reviewable command-by-command.

### 2.3 Frontend layering rules

- **No std blocking on the render thread.** All service calls go through `tokio` (`gpui::AsyncApp` / background tasks), results land in GPUI entities via main-thread updates.
- **Theme system:** port the CSS variable set into a `Theme { colors, radii, shadows, font }` struct; `ThemePreset` (22), `AccentColor` (17), `FontPreset` (6) switch it at runtime by mutating a shared entity — pixel values copied from `globals.css` and `theme-preset.ts`/`accent-color.ts`/`font-preset.ts`, not reinvented.
- **Components:** GPUI widget zoo mirroring `src/components/ui/*`: Button (primary/ghost/danger; sm/md/icon), Badge, Dropdown (outside-click close), GlassPanel/Card, Icon (81 embedded Phosphor Bold SVG paths from `src/lib/icon-data.json`), Skeleton, Switch, Tooltip (350 ms delay). All spacing/sizing values taken verbatim (h-9 title bar, w-21 sidebar, px-8 pb-8 content, dialog max-w scale z tiers, etc.).
- **Screens** are ported screen-by-screen with side-by-side visual diffing against the running Tauri app (both apps open next to each other at 1280×800), then interaction parity checks.

### 2.4 What replaces each WebView technology

| WebView feature | Native replacement (decision) |
|---|---|
| `backdrop-filter` glassmorphism | GPUI translucent fills + **backdrop blur where GPUI provides it** (window `BackgroundAppearance::Blurred`, blur-bearing styles); verify exact look in GPUI 0.2.2 and if unavailable fall back to pre-sampled blur: render the composited background into a GPUI surface texture blurred by a small GPU pass. Visual fidelity is the acceptance bar, not the mechanism. |
| Frameless window / drag region | GPUI undecorated window + manual hit-test drag/maximize on the h-9 title bar. |
| HTML5 video (login bg, recordings) | Login background → bundled/webp fallback or a ffmpeg-next/odran decode loop → GPUI texture when verified; recordings player → native reversed review via ffmpeg frame pipeline (`recorder/win` already links Media Foundation — reuse for decode) — Phase 4 item, flagged. |
| skinview3d (WebGL) | Skin viewer built on a `wgpu`/GPUI texture child surface; skin/cape PNGs are already Rust-side. Poses: idle/walk/run/fly/crouch reimplemented (Phase 4). |
| Canvas skin compositing (body icon) | CPU compositing in Rust → GPUI image (straightforward pixel code). |
| marked + DOMPurify readme | Rust markdown → GPUI styled rich text covering the `.md-readme` subset (headings, lists, tables, code, blockquote, links); sanitize by construction (no HTML pass-through). |
| Web Audio (spooky SFX, music, cues) | `rodio`/CPAL: port synth oscillators 1:1 (sounds are procedural — deterministic port) or pre-render bytes; keep autoplay-unlock via first-input resume. |
| SVG particle engine + ghost trail | GPUI painter: same seeded PRNG + same per-effect instance counts/colors/curve parameters ported from `theme-effects.tsx`; SVG glyphs re-authored as GPUI paths; blur-glow approximated with layered translucent strokes. |
| `convertFileSrc` asset loading | GPUI async image loader with the same scope rules (`$APPDATA/**`, `$HOME/**`), incl. data-URI/base64 favicon decoding. |
| plugin-dialog open/save | `rfd` (native file dialogs) behind the same API names. |
| plugin-shell open | std/rfd `open` per platform (same behavior). |
| clipboard text + PNG image | `arboard` (text + image). |
| global shortcuts (recorder) | native hotkey crate (e.g. `global-hotkey`), same settings semantics (validated unique combos, only active while a game runs). |
| deep link `arcane://join` | Windows registry registration + IAssociationExperience? → use Windows `RegisterProtocolNotification`-equivalent via existing `windows` crate + Linux `.desktop` `MimeType=x-scheme-handler/arcane` (xdg rebuild); parse with `url` crate (`parse_join_url` reusable as-is). |
| single instance | native named mutex (Windows) / lockfile+socket (Linux) → focus existing window. |
| `tauri_plugin_updater` | Native updater in `frontend/src/updater/`: R2 manifest layout kept byte-compatible (`latest.json`, `beta/latest.json`, `versions/<v>.json`, `releases.json`), minisign-verify with the **same pubkey** (from tauri.conf.json), download with existing `DownloadManager` progress → `updater://progress`, Windows NSIS passive install of same installer artifact + `install_on_close` gating, Linux AppImage self-install path (package-manager installs update via pacman/AUR as today). |
| Microsoft login window | **DEFAULT: open the user's default browser** at the same MS authorize URL (PKCE identical), redirect to the same `login.microsoftonline.com/common/oauth2/nativeclient` while a tiny localhost listener/redirect-catcher harvests `code`. All token/XBL/XSTS/profile + refresh code is reused untouched; `auth://progress` emitted from the Rust side exactly as today. Alternative (kept open as fallback): spawn a small embedded webview child window **only** for login (a single wry window — does not make the app a webview app). Decision needed from you (see §6). |
| webview memory trick | dropped (not needed). |
| `localStorage` (14 UI keys) | a small JSON file (e.g. `ui_state.json` in the same data dir) + service persistence where a backend field already exists (`selected_instance_id` already lives in LauncherSettings). |
| React Query caching | GPUI store layer with matching staleTime/refetch semantics where visible in the UI (e.g. version list 1 h cache via `JsonCache` already on the backend). |

---

## 3. Migration phases (incremental, Tauri app remains runnable throughout)

### Phase 0 — Workspace scaffold
Create `native/` workspace + both crates; CI-style `cargo check` green; `frontend` opens an empty 1280×800 undecorated GPUI window titled "Arcane Launcher" with the app icon; fonts (6 families × 400/500/600/700 weights) bundled; theme structs compile with the obsidian palette + accent system loaded from real values.

### Phase 1 — Core extraction (backend, no UI) — ✅ DONE 2026-11-10 (commit 9a8ee86)
Copy pure services verbatim into `backend`; strip the mechanical Tauri bits per §1 ("needs adapter" list); introduce `EventSink`; unit-test parity: port any `tests` from src-tauri, plus new chantensets for `build_launch_plan`, manifest resolution, settings migration (all currently covered by doc-tests/tests in the source crate). **Acceptance: `cargo test -p backend` green; launch plan output byte-comparable with the Tauri build for a sample instance.**

Committed state: all ~87 pure files verbatim; adapters done for `connectivity` (NetworkMonitor probes + NetworkStatus event), `download/manager` (DownloadProgress), `minecraft/launch`+`mod.rs` (`&AppState`, LaunchStarted/Log/Exit), `minecraft/auth` (**`LoginHost`/`LoginWindow` traits** — desktop shell plugs in the embedded OAuth window; `pub const LOGIN_WINDOW_LABEL = "ms-login"` is the window contract), `discord_rpc` (join/request events via sink), `join_link` (`pub use url::Url`, manual `Clone` for `PendingJoin`, window-focus moved to the shell's open-urls handler), `update_on_exit` (statics-only; close interception → Phase 5), `recorder` (**`Hotkeys` trait** + `DisabledHotkeys` no-op + local `parse_hotkey` replacing the plugin's `Shortcut` parser; AppState owns `recorder` + `hotkeys: Arc<dyn Hotkeys>`), `paths.rs` (dirs-crate data dir, same `com.arcanelauncher.app` root), `state.rs` (`AppState::initialize(events, hotkeys)`), `build.rs` (bakes MSA client id + Discord app id from `src-tauri/*.key`). `commands/*` ported with `&AppState` signatures (`login_microsoft(state, &dyn LoginHost)`); `updates.rs` dropped with the updater → Phase 5. `windows` feature `Win32_System_Variant` added explicitly (was fed transitively by Tauri deps for `IMFSourceReader::GetPresentationAttribute`). **Tests: 68 passed / 2 ignored (live-record hardware test).**

### Phase 2 — App shell + auth
Login screen (pixel-matched) → browser-login flow → shell gate states; title bar + sidebar + top bar; background stack (image/overlay opacity/glow blobs/gradient); page routing with 220 ms transition; toasts; banners; account switcher. Launch a Minecraft instance end-to-end from the native shell (vanilla first) with console, running list, log streaming, exit handling. **Acceptance: launcher boots, logs in, launches/streams/stops a game, Discord RPC idle+game presence identical.**

Committed state (chrome + auth): title bar/sidebar/top bar + background stack + gate render from `AuthStore` (`get_session` → Unknown/LoggedOut/LoggedIn); full-screen login screen (`login-screen.tsx` + `auth-panel.tsx`) with the hero panel (ffmpeg-extracted `login-bg.webm` first frame at 16% opacity — gpui cannot loop video; runtime-rasterized 56px grid + feTurbulence-noise textures, drift animations static), the glass sign-in panel, Microsoft sign-in through the tao+wry embedded `LoginHost` (`login_host.rs`, window "Sign in with Microsoft" 480×640, own thread + event loop) and offline sessions; hand-rolled single-line `TextField` on gpui's `EntityInputHandler` IME contract (gpui 0.2 has no input widget; static caret, no selection/undo); toasts (`toast-store.ts` + `toaster.tsx`, 4s auto-dismiss); account switcher (`account-switcher.tsx`: switch/remove/sign-out/add-account modal, mc-heads avatars — skin-library faces are Phase 5, group-hover trash always visible muted→danger). Backend bridge (`bridge.rs`): dedicated tokio runtime thread runs `AppState::initialize`, commands spawn on its handle and are awaited from gpui tasks; `auth://progress` events drain through a smol channel into `AuthStore`. Deferred: letter-spacing, text gradients (solid accent for `.accent-gradient-text`), spin/blink animations, memory chip, network live state, page routing.

Committed state (launch slice — first Phase 3 screen, `page.tsx` + instance components): Play page mounted on LoggedIn (`play.rs`) — hero banner with glow-orb stand-ins for the SVG strokes (gpui has no blur/transform), 5 stat tiles mirroring `stat_tile` (primary instance = selected ?? max_by_key last_played, count-up 500ms cubic ease-out), active-download bar with pause/resume/cancel + percentage/ETA, recent-instance cards, empty state; Play/Configure buttons per card and banner (`launch.rs` `LaunchButton`, two maps banner/card mirroring React's separate button instances). Modal architecture (`modals.rs` `ModalStore` global, shell renders dialog entities per spec): Smart Launch dialog (pre-launch health check via `check_instance_health`, per-item one-click fixes through `apply_crash_fix`, "Fix All" via `fix_instance_health`, launch-on-ready) and Launch Validation dialog (`validate_instance` → `fix_instance` fixes, "Launch anyway"). Launch flow: Play → Smart Launch → begin_launch (auto-update pass `apply_all_mod_updates` with "Updating…" phase + toast, then `run_launch`) → on fail validation dialog; ready path calls `launch_direct`. Instance console (`console.rs`): `LogStore` global with pending buffer + 100ms flush tick, tone classification on arrival, 5000-line cap; per-open seed from `get_instance_log`, copy to clipboard, clear, stop (`kill_instance`), auto-scroll with at-bottom detection (24px), process stats polled every 2s (`get_process_stats`). Running list: `RunningStore` global seeded by `list_running_instances` + LaunchStarted/LaunchExit events; header running count + 1s clock tick while a primary instance runs. Downloads bridge: `DownloadsStore` global, DownloadProgress event merge preserving instance_id/url. Deferred (documented in code): HauntedLaunch, CrashHelperDialog ("Find the problem"), Share log, record button, instance settings dialog (Configure), servers/worlds/screenshots/recordings content tabs, instance switcher caret dropdown, empty-state Create button, banner SVG strokes (glow-orb approximation), running-dot pulse, progress-fill glow shadow, selected-instance persistence across restarts.

Committed state (instances slice — second Phase 3 screen, `src/app/instances/page.tsx`): page routing (`shell.rs` `Page` enum — sidebar Play/Instances links routable via `on_click`, the four unported routes stay inert; pages are created on first visit and kept alive across navigation, dropped with the gate in `drop_play_state` which also resets `page` and clears the delete/duplicate dialog specs; top-bar breadcrumb from `Page::breadcrumb()` — HOME/INSTANCES/MODS/DOWNLOADS/"ARCANE LAUNCHER" (/skin fallback)/SETTINGS). Instances page (`instances.rs`, ~2400 lines): header (glow-orb backdrop, 44px gradient icon box, title + stats row with the count-up animation), toolbar (transparent search input via the new `text_input::FieldStyle`/`render_styled`, grid/list view toggle, sort select as a custom dropdown — both dropdowns open upward because gpui paints in document order), skeleton ×6 / empty state / no-match states, group sections (first-appearance order, ungrouped last) with caret + count, grid cards (135° gradient header fallback = the React null-icon branch, favorite star 24px on black/40, loader badge except Vanilla, per-card validation line, interrupted-migration banner with Restore/Dismiss through `commands::evolution::resolve_interrupted_migration`, playtime count-up per card, hover action row with Play/Console/menu) and list rows; ⋮ menu (Console/Duplicate/Remove only — boost/version-change/history/export deferred) with an in-card occluding backdrop; Delete dialog (native dark checkbox 13px, disabled+dimmed for imported) and Duplicate dialog (name input prefilled `"(copy)"` with React `autoFocus` via first-render `window.focus`, error line, busy-disable) built from `ModalStore` `DeleteSpec`/`DuplicateSpec` and rendered by the shell above everything like the launch dialogs. Shared selection: `bridge::SelectionStore` global replaces PlayPage's local `selected` (the instances page's Select buttons write it; PlayPage observes it → `after_instances_changed` + notify, and `primary()` reads it — the React `selectedInstanceId` zustand subscription). Backend: `commands::system::open_path` (open-folder, explorer/`xdg-open`); 10 new Phosphor-bold icons. Deferred (documented in `instances.rs` module docs): create-instance/import dialogs (header + empty-state New-instance buttons omitted), Manage mods (needs the Mods page), card thumbnails + instance icons, pack-update banner (needs network-status plumbing), hover lift/shadow/accent-hairline (no transforms in gpui 0.2.2), pulse animations (static), sort-menu outside-click close. **Tests: 70 passed / 2 ignored; clippy clean; boot smoke test ok.**

### Phase 3 — Core screens
Home (banner, stat tiles, download panel, content tabs, recent instances), Instances (grid/list, sort/search, create incl. DownloadConfirmStep, duplicate/delete/settings dialogs, evolution + history wizards, import), Mods (browse/filters/details/installed + install dialog + collection import + profiles + resource pack order), Downloads page, server/world/screenshot sections. Every `Event` and command from the contract table gets a call site. **Acceptance: feature-checklist review of §1 §6.3 command list (all ~110 called).**

### Phase 4 — Specialty rendering
Skin Studio (skin library tabs, compositing, MineSkin, wgpu 3D viewer, poses), recorder UI (record button, gallery replay with player), markdown readme, glass/blur refinement pass, particle effects + halloween package (sounds via rodio, ghost trail, music toggle, prompts, yearly scare), `prefers-reduced-motion` parity. **Acceptance: side-by-side screenshot diff vs Tauri app on obsidian + halloween themes.**

### Phase 5 — Platform surfaces
Settings (7 categories incl. graphics panel GPU preference, recording panel, storage/cleanup, about/update panel), native updater (check/download/install/rollback + What's New + install-on-close), deep links, single instance, global shortcuts hotkeys, Linux packaging (deb already produced by cargo-bundle? decide: same NSIS/deb/AppImage artifacts as today via cargo-packager or manual), Windows NSIS release pipeline parity with `build-signed.ps1`. **Acceptance: signed Windows installer runs, installs over existing Tauri-installed data dir without touching it, updater round-trip against staging endpoint.**

### Phase 6 — Parity QA + switchover
Full manual matrix (both launchers side-by-side on Windows + a Linux VM): auth, launch all 5 loader kinds, instance CRUD/migrate/import, modrinth+curseforge flows, downloads/java/skins/recorder/discord/persistence. Perf/memory baseline vs webview app. Only then: remove the Tauri frontend from release builds (keep sources in repo for reference per your rule), flip default build target.

---

## 4. Explicit non-goals / guarantees

- **No UI redesign** — every color, spacing value, icon path, animation curve is lifted from the existing sources, and screens are diffed against the Tauri app, not against mockups.
- **No placeholder implementations** — each phase's acceptance is a working feature, not a stub.
- **No backend rewrites** — services are copied; only the enumerated adapters change.
- **Existing user data is untouched** — same data dir, same filenames, same settings schema (serde defaults already handle migrations).
- Windows + Linux (X11/Wayland) maintained; macOS possible later (GPUI supports it) but not a target.

---

## 5. Risks & mitigations (top 8)

1. **GPUI blur/glass fidelity** — the whole UI leans on backdrop blur. Mitigate: prototype the glass panel on Phase 0/2 before porting screens; pre-sampled blur fallback if needed.
2. **Published `gpui 0.2.2` lags zed upstream**; if a needed API is missing, pin a git dependency on a tagged zed revision (documented decision point per phase, no invented APIs).
3. **Microsoft login UX change** (browser vs embedded) — see §6.
4. **Video decode** for recordings player — heaviest Phase-4 item; Media Foundation reuse vs ffmpeg decision deferred to that phase.
5. **Markdown readme subset** — long-tail readme formatting can't all be perfectly reproduced; scope: match `.md-readme` CSS coverage, scrub anything else.
6. **Updater rewrite** is the riskiest backend piece — test against staging R2 before Phase 5 sign-off, keep Tauri updater able to ship until then.
7. **Particle/animation perf** at 60 fps with 70+ sprites — GPUI painter batching handles it, but budget a perf pass in Phase 4.
8. **Icon parity** — 81 exact Phosphor Bold paths are already extracted (`icon-data.json`); hand-drawn SVGs (bat/pumpkin/ghost/cauldron/rune ring) need re-authoring — flag early in Phase 2/4.

---

## 6. Decision — RESOLVED 2026-11-08

Microsoft login will use **Option B: an embedded login window** (a single small browser window spawned only for the OAuth dialogue — closest to the current UX). Everything else in the shell is GPUI. `minecraft/auth.rs::interactive_authorize` gains a host adapter on Windows (embedded window) with the identical PKCE → `nativeclient` redirect harvest, window-Destroyed = cancel, 600 s timeout, and `auth://progress` emission unchanged.

**Current Phase 0 note:** this decision was made after analysis was complete, and is consistent with the analysis. The Phase 2 acceptance criteria for auth stand unchanged. The Windows adapter for the embedded login host is part of Phase 1's `backend` `auth` port boundary (event sink) + Phase 2 (GPUI-side host spawn), and the Phase 2 acceptance checklist in §3 phases 2 must include the embedded-browser parity before sign-off.
