# Security Policy

Thank you for helping keep Arcane Launcher and its users safe.

## Supported Versions

Only the **latest release** of Arcane Launcher and the current `main` branch
receive security fixes. Older releases and tags are not supported — please
update to the newest version before reporting an issue.

| Version                | Supported |
| ---------------------- | --------- |
| latest release / main  | ✅        |
| older releases / tags  | ❌        |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Report privately using GitHub's
[Private Vulnerability Reporting](https://github.com/Arcane-Launcher/Launcher/security/advisories/new)
feature ("Report a vulnerability" on the repository's Security tab). This keeps
your report confidential and lets us coordinate a fix before public disclosure.

### What to include

To help us reproduce and fix the issue quickly, please include:

1. A clear description of the vulnerability and its potential impact;
2. The affected version, commit hash, and platform (Windows / Linux);
3. Step-by-step reproduction instructions, ideally with a minimal example;
4. Any relevant logs, screenshots, or proof-of-concept details;
5. Your assessment of severity, if you have one (optional).

### What to expect

- We aim to acknowledge reports within **72 hours**;
- We will investigate, keep you informed of progress, and publish a fix as
  soon as practical;
- Once a fix is released (or if the issue is declined with an explanation),
  we will coordinate with you on any public disclosure;
- If you wish to be credited for the report, say so in your submission and we
  will include your name or handle in the release notes or advisory;
- We ask that you give us a reasonable time window to address the issue
  before any public disclosure, and that you do not exploit the issue beyond
  what is necessary to demonstrate it.

## Scope

### In scope

- Vulnerabilities in the launcher application itself (authentication handling,
  credential storage, process launching, file handling, network clients,
  update mechanism, and so on);
- Vulnerabilities in the build or release pipeline as it lives in this
  repository.

### Out of scope

- Vulnerabilities in third-party dependencies that do not affect this project
  in practice, or that can only be fixed upstream — we will still track and
  assess these, but may not be able to patch them ourselves;
- Social engineering, phishing, or attacks against our users rather than the
  software;
- Denial-of-service through volume attacks against public services;
- Automated scanner output without a demonstrated, exploitable issue;
- Issues in Minecraft, Mojang, Microsoft, Modrinth, or CurseForge services.

## Safe Harbor

If you make a good-faith effort to comply with this policy during your
research — reporting issues promptly and privately, and avoiding privacy
violations, data destruction, and service degradation — we will consider your
conduct authorized and will not pursue action against you for it.
