# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Per-site sandboxes. Every site runs in its own Chromium profile (cookies, storage, cache, connections), with a provider's own sites grouped together.
  - Tabs move between sandboxes when they change site, and back/forward works across those moves.
  - Settings → sites lists every sandbox with a "forget" button.
- Sign-in sharing copies the provider's sign-in into the site's sandbox when you allow it.
- Cookie-consent banners are removed (uBlock filters – Cookie Notices, EasyList Cookie).
- Windows: Chromium's sandbox. The bundle's `unskilled.exe` is now CEF's `bootstrap.exe` (with our icon and name), which sandboxes the renderer, GPU and utility processes and runs the browser from `unskilled.dll`.

### Fixed
- Windows: the vault kept nothing across restarts, because archive paths used `\`.
- Windows: logging off or shutting down with the browser open saves the vault, as on Linux. Before, only closing the window did. Ctrl+C and closing the console of a debug build save it too.

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
  - an extensions panel with pause, site exceptions and a tracker log.
- An encrypted vault with a PIN on every launch:
  - TPM-sealed on Linux, passphrase fallback;
  - history and cache wiped on close.
- A sign-in gate for Google, Apple, Microsoft, Facebook, GitHub, Discord, X and LinkedIn.
- A password manager:
  - PIN before every use (optional);
  - import from Google Password Manager, Chrome and Firefox.
- Bookmarks: ctrl+d, a bookmarks page, start-page tiles, and HTML import.
- `unskilled://settings`: search engine, sign-in decisions, passwords, cookies.
- Docked Chrome DevTools and a site inspector (IP, ASN, CDN, TLS, hosts, blocked trackers).
- Mullvad connection status in the bar.
