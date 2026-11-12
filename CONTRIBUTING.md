# Contributing to Arcane Launcher

Thank you for your interest in contributing to Arcane Launcher! This document
explains how to set up a development environment, the standards the codebase
follows, and the process for getting changes merged.

## Licensing Note

Please read [`LICENSE.md`](LICENSE.md) first. Arcane Launcher is a proprietary,
source-available project. By submitting a contribution (pull request, patch, or
any other material intended for inclusion), you agree to the contribution terms
in Section 7 of the license: you **retain ownership** of your contribution, but
you grant the project maintainer a perpetual, worldwide, royalty-free,
irrevocable license to use, modify, relicense, and distribute it as part of
Arcane Launcher.

## Repository Layout

- `native/` — the native Rust + GPUI desktop application (workspace with
  `crates/backend` and `crates/frontend`);
- `src-tauri/` — the original Tauri-based application, kept as the reference
  implementation for behavior and UI. **Do not modify it** when working on the
  native app — treat it as source material only;
- `.github/` — CI workflows and repository configuration;
- `packaging/` — platform packaging helpers.

## Getting Started

1. Install a recent stable Rust toolchain (the workspace uses the 2024
   edition);
2. Fork or branch from `main`;
3. Build and test the native app:

   ```sh
   cd native
   cargo check -p backend -p frontend
   cargo test --workspace
   cargo clippy --workspace --all-targets
   cargo fmt --all
   ```

All four commands must pass before you open a pull request. CI runs exactly
these checks on Windows and Ubuntu, so a green local run is a strong predictor
of a green CI run.

## UI Rules

The native app's UI must be **pixel-identical** to the original Tauri app:
same layout, colors, fonts, spacing, icons, and interactions. When changing
anything user-facing:

- Match the reference implementation in `src-tauri/` exactly, down to spacing
  and hover behavior;
- Do not redesign components or "improve" visuals as a side effect of other
  changes;
- If you find a discrepancy between the native UI and the original, fix the
  native side to match the original (or raise it in an issue if you believe
  the original is wrong).

## Coding Standards

- No placeholders, stubs, or TODO-only implementations — features must be
  complete and functional;
- Keep changes consistent with the structure, naming, and style of the
  surrounding code;
- Prefer small, focused pull requests over large mixed ones;
- Add or update tests for backend behavior you change;
- Keep warnings out: `cargo clippy` must stay clean.

## Commit and Pull Request Style

- Use short, descriptive commit messages in the imperative mood (for example,
  `Fix: Linux-leg dead code in recorder`, `Native UI: Instances page`);
- One logical change per commit;
- In your pull request description, explain **what** changed, **why**, and
  **how you verified it** (tests run, manual checks on Windows/Linux);
- Keep changes scoped to `native/` (plus `.github/` when the change is
  CI-related) unless the maintainers agree otherwise;
- A maintainer must review and approve before merge; CI must be green.

## Reporting Bugs and Suggesting Features

- **Bugs**: open a GitHub issue with a clear title, reproduction steps,
  expected vs. actual behavior, and your platform and version. See
  [`SECURITY.md`](SECURITY.md) before reporting anything you believe has
  security impact — those go through private reporting, not public issues.
- **Features**: open an issue describing the problem you want solved and the
  proposed behavior before writing significant code, so we can agree on the
  approach first.

## Code of Conduct

Be respectful and constructive in issues, discussions, and reviews. Keep
criticism technical and aimed at the code, not the person. Maintainers may
remove content or restrict participation that is abusive or disruptive.
