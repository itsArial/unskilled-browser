# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-30

First public release.

### Added
- Chromium 156 (CEF), rendered windowless with zero-copy GPU frames:
  - Linux: DMA-BUF → Vulkan.
  - Windows: D3D11 → D3D12.
- A browser interface drawn from scratch:
  - tabs, address bar, start page;
  - spring animations;
  - a loading ring traced into the address bar's outline.
- A built-in blocker:
  - uBlock Origin lists, scriptlets and redirect resources;
  - YouTube anti-enforcement;
  - cookie-consent banners removed (uBlock filters – Cookie Notices, EasyList Cookie);
  - an extensions panel with pause, site exceptions and a tracker log.
- An encrypted vault with a PIN on every launch:
  - TPM-sealed on Linux, passphrase fallback;
  - history and cache wiped on close;
  - saved cleanly on logout and shutdown (SIGTERM on Linux, the end of the session on Windows).
- Per-site sandboxes. Every site runs in its own Chromium profile (cookies, storage, cache, connections), with a provider's own sites grouped together.
  - Tabs move between sandboxes when they change site, and back/forward works across those moves.
  - Settings → sites lists every sandbox with a "forget" button.
- Chromium's process sandbox on both platforms. On Windows the bundle's `unskilled.exe` is CEF's `bootstrap.exe` (with our icon and name), running the browser from `unskilled.dll`.
- A sign-in gate for Google, Apple, Microsoft, Facebook, GitHub, Discord, X and LinkedIn.
  - Allowing a sign-in copies just that sign-in into the site's sandbox.
- A password manager:
  - PIN before every use (optional);
  - import from Google Password Manager, Chrome and Firefox.
- Bookmarks: ctrl+d, a bookmarks page, start-page tiles, and HTML import.
- `unskilled://settings`: search engine, sign-in decisions, passwords, cookies.
- Docked Chrome DevTools and a site inspector (IP, ASN, CDN, TLS, hosts, blocked trackers).
- Mullvad connection status in the bar.
