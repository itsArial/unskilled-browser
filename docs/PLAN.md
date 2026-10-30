# unskilled browser — plan

A personal, privacy-first browser. Rust shell, custom GPU UI, Chromium engine,
every website in its own sealed sandbox with its own identity and its own exit IP.

---

## 1. Core decisions (and why)

### Engine: Chromium via CEF 156 (`cef` crate, tauri-apps/cef-rs)

Writing a web engine is not a one-person project (Servo has been at it for 12+ years
and still can't run most of the web). Of the realistic options only Chromium gives us
the three things asked for:

| need | Chromium/CEF | Servo | WebKitGTK |
|---|---|---|---|
| Chrome extensions | yes | no | no |
| Real Chrome DevTools | yes | no | Web Inspector |
| Looks like "a Chrome" on the wire (TLS/JA4, H2 settings) | yes, natively | no | no |
| Site isolation / mature sandbox | yes | partial | yes |

The last row matters for privacy more than people think: a browser that *claims* to be
Chrome but has a non-Chrome TLS handshake is instantly unique. Being actual Chromium
means the network fingerprint is already the most common one on the internet.

### UI: our own toolkit on the GPU, web pages composited as textures

Pages are rendered **off-screen** by CEF straight into GPU memory (DMA-BUF, zero-copy on
Linux) and imported as `wgpu` textures. Our compositor draws them like any other layer.
That is what makes "butter" possible: tab switches, split view, overview grids etc. can
animate the live page itself, not a screenshot.

"From scratch" boundary — we write: widget tree, layout, animation/physics, compositor,
renderer, shaders, theming, the dot-matrix font, input routing, every control.
We use only platform plumbing: `winit` (window + raw input from the OS), `wgpu` (GPU API),
and a glyph shaper/rasterizer for body text (`swash`/`rustybuzz` — re-implementing
OpenType shaping is months of work with zero visible benefit). No UI framework, no webview UI.

### The one real technical risk

CEF only exposes the full Chrome **extension** system on "Chrome-style" browsers, and
off-screen rendering is only available on "Alloy-style" browsers. Since M128 both styles run
on the same Chrome bootstrap, so extension service workers/content scripts are expected to
run, but toolbar actions/popups have no built-in UI in Alloy style (fine — we draw that UI
ourselves and render popup pages in our own OSR view). **This must be proven in the P0 spike.**
Fallback ladder if it doesn't work: (a) small CEF patch — we'll maintain our own CEF build for
privacy patches anyway (P6); (b) host extension pages ourselves via `chrome-extension://` URLs.

---

## 2. Architecture

```
┌─────────────────────────── browser process (Rust) ───────────────────────────┐
│  ui        own toolkit: scene, layout, springs, renderer, dot font, text      │
│  shell     tabs, omnibox, panels, palette, settings — all drawn by `ui`       │
│  engine    CEF wrapper: OSR browsers, input forwarding, request interception  │
│  shield    sandboxes, personas, blocker, URL cleaning, permissions            │
│  tunnel    Mullvad daemon client, per-sandbox SOCKS exits, kill switch, DoH   │
│  wire      network inspector: CDP + NetLog + proxy taps, TLS keylog, pcapng   │
│  vault     encrypted storage (argon2id + XChaCha20-Poly1305), keyring, wipe   │
└───────────────────────────────────────────────────────────────────────────────┘
        │ CEF IPC
┌───────┴───────── Chromium child processes (sandboxed: namespaces + seccomp) ─────┐
│ renderer per site (strict site isolation) · GPU · network service · utilities   │
└──────────────────────────────────────────────────────────────────────────────────┘
```

Crates live in `crates/<name>`; each lands when its phase starts (no empty shells).

---

## 3. Privacy model

### Profiles → Site sandboxes → Personas

- **Profile** = like a Chrome profile: separate everything, own settings, own extensions,
  optional password lock.
- **Site sandbox** = inside a profile, every registrable domain (eTLD+1) gets its **own CEF
  RequestContext** with its own disk directory: cookies, cache, localStorage, IndexedDB,
  service workers, HSTS, permissions. `github.com` cannot see one byte of what
  `google.com` stored, not even via redirects or embedded iframes — they're different
  browsers as far as Chromium is concerned. Logins persist per sandbox.
- **Persona** = each sandbox gets a stable, coherent fake identity generated once:
  - Chrome version/channel, OS + platform, screen size, DPR, CPU cores, device memory,
    GPU vendor/renderer strings, languages, timezone (matched to its exit city),
    fonts list, canvas/audio/WebGL noise seed.
  - Applied in layers: (1) headers — `User-Agent`, `Sec-CH-UA*`, `Accept-Language`;
    (2) JS surface — injected in the renderer before page scripts (`OnContextCreated`);
    (3) deep surface (workers, fonts, canvas internals) — Chromium patches in our own build.
  - We impersonate **other Chrome configurations, not Firefox/Safari.** A Blink engine
    pretending to be Gecko is caught by one feature-detection line and by the TLS handshake,
    which makes you *more* unique. Coherent Chrome variants blend into the biggest crowd.
- **Burner** sandboxes: in-memory only, new persona + new exit, gone when the tab closes.
- **Rules**: "always open `*.mybank.nl` in persona X via NL exit".

### Network (Mullvad)

- **Mode A — system Mullvad** (installed, v2026.5, currently connected): talk to the daemon
  (`mullvad` CLI first, gRPC management socket later) for status, connect/disconnect, relay
  choice. Browser refuses to load anything when the tunnel is down (kill switch).
- **Mode B — per-sandbox exits**: while the tunnel is up, Mullvad's SOCKS5 proxies are reachable
  inside it (`10.64.0.1:1080` for the current relay, plus per-relay SOCKS hosts for other cities).
  Each sandbox gets its own proxy → **every site sees you from a different IP/city.**
  DNS goes through the proxy (SOCKS5h), QUIC disabled on proxied contexts.
- **Mode C — browser-only tunnel** (later): your Mullvad account number stored in the vault;
  the browser registers its own WireGuard key via the Mullvad API and runs WireGuard in-process
  (`boringtun` + userspace TCP/IP). Browser traffic is tunnelled even if system VPN is off.
- **Tor** (later, optional): `arti` (Rust Tor) as an alternative per-sandbox transport, `.onion` support.
- WebRTC locked to proxy-only (`disable_non_proxied_udp`) → no real-IP leak.

### Hygiene (defaults, all on)

- No Google services: no API keys, Safe Browsing pings, sync, variations/field trials,
  component updater, spellcheck downloads, translate, crash uploads, metrics.
- Built-in blocker (EasyList/EasyPrivacy/uBO-style lists) on every network request.
- Tracking-param stripping (`utm_*`, `fbclid`, `gclid`, `mc_eid`, …), AMP → canonical,
  bounce-tracker detection, cross-site `Referer` trimmed to origin.
- HTTPS-only with explicit per-site downgrade.
- All permissions default-deny, per sandbox.
- Upload sanitising: strip EXIF/GPS/metadata from images on file upload.
- Copy-clean: URLs copied from the omnibox are stripped of trackers.

### Honest limits

No browser makes you untraceable. Logging in to a site tells that site who you are
(isolation only stops it from following you elsewhere). Behaviour and timing correlation exist.
CEF-level persona spoofing can be detected by a determined fingerprinter until the deep layer
(own Chromium patches) lands. Every extension you install is code that can see pages — we'll
show per-extension sandbox access and default extensions to "off in new sandboxes".

---

### Shield — built-in blocker (built)

- Brave's `adblock-rust` engine running uBlock Origin's own lists (filters, badware, privacy,
  quick fixes, unbreak, tracking parameters) + EasyList, EasyPrivacy, Peter Lowe, URLhaus and the
  Adblock Warning Removal List. ~257k rules, built in ~0.3 s (release). uBO lists refresh every
  4 h, third-party lists daily.
- uBO's 151 scriptlets + 46 redirect resources are extracted from uBO's source *as text*
  (`crates/shield/src/bin/ubo-resources.rs`, never executed) into `assets/resources.json`;
  uBO's own lists get "trusted" permission, which YouTube's `trusted-*` rules need.
- Network: block, neuter (serve a harmless stand-in such as a no-op `gpt.js`, so sites probing
  for ad scripts don't detect the blocker), or clean (strip tracking parameters).
- Page: at V8 context creation the renderer fetches the page script synchronously over a
  CSP-bypassing private scheme and evaluates it natively — scriptlets run before any page script,
  hiding uses a constructed stylesheet (not in the DOM), generic class/id hiding and procedural
  filters run from a closure with a native `query` the page can never reach.
- YouTube: uBO's `json-prune`/`trusted-edit-inbound-object`/`replace-node-text` rules remove
  player ads and defuse the "ad blockers are not allowed" enforcement. The page script request
  waits for the filters on startup, so no page ever runs a script unprotected. No blocker can
  promise YouTube never changes faster than the lists; fresh lists are the defence.
- Extensions panel: per-site protection toggle (exception), pause everywhere, list status/update,
  per-page attempt log with rule, list, category and time for every blocked request.

### Vault — encrypted profile behind a PIN (built)

- While running, the Chromium profile lives in RAM (`$XDG_RUNTIME_DIR`, tmpfs) — plaintext never
  touches the disk. At rest it's one encrypted archive (XChaCha20-Poly1305).
- Unlock screen on every launch, drawn by our UI before Chromium starts; filter lists build while
  the PIN is typed. Single instance per vault (flock).
- A short PIN alone is brute-forceable offline, so the key is sealed in the laptop's TPM 2.0 with
  the PIN as its auth value: the TPM's dictionary-attack lockout makes guessing impractical and the
  archive is useless on another machine. Fallback without TPM: argon2id passphrase.
- Only an allowlist survives a restart (`crates/vault/src/archive.rs`): cookies, Local Storage,
  IndexedDB, service-worker registrations, extension data, our settings. History, visited links,
  favicons, sessions/tabs, caches, HSTS/DNS state, site-engagement prefs, media-device salts and
  anything Chromium adds in future are dropped by construction. A fresh `Preferences` with
  "continue where you left off" is written each start — that's what makes Chromium keep session
  cookies (logins) without restoring any tabs.
- Encrypted checkpoint every 10 min; SIGTERM/SIGINT/SIGHUP (logout, shutdown) trigger the same
  clean save (on Windows: WM_ENDSESSION, saved before the message returns, and console
  Ctrl+C/close). The previous archive is kept as a fallback; a RAM session left by a crash is
  recovered after the next successful unlock.
- (The disk is already LUKS-encrypted; that only helps while the machine is off.)

### Debug panel — site inspector (built: `</>` button or Ctrl+Shift+D)

- The IP/port/protocol/TLS/headers Chromium actually used, per request, from its own DevTools
  protocol (a CDP observer per tab — `crates/engine/src/inspect.rs`).
- ASN / ISP / hosting company from an offline database downloaded through the tunnel — looking an
  IP up on a third-party service would itself leak where you browse.
- CDN / reverse-proxy detection (Cloudflare, Akamai, Fastly, CloudFront, …) from the ASN plus
  response headers (`cf-ray`, `x-served-by`, `via`, …) vs. a direct origin server.
- TLS version/cipher/certificate, your Mullvad exit (from `mullvad status -v`), and the tracker
  attempt log with full request details. Network database: `crates/netinfo` (iptoasn.com, public
  domain), fetched on first use and weekly.
- Chrome DevTools (F12, Ctrl+Shift+I, right-click → Inspect) docks next to the page. CEF can't
  render DevTools windowless, so the bundled frontend loads as a normal page and talks to the tab
  through `crates/engine/src/devtools.rs`: a WebSocket on 127.0.0.1 behind a random 128-bit path
  relaying CDP in-process — never Chromium's global `--remote-debugging-port`.
- Right-click menus are drawn by us from Chromium's menu model (+ open link in new tab, copy link,
  inspect).

## 4. Security stack

- Chromium sandbox on (user namespaces + seccomp-bpf), strict site isolation + OOPIFs.
- **Strict mode** per sandbox: V8 jitless, no WebAssembly, no WebGL/WebGPU, no JIT-able surfaces.
- Vault: everything we persist (history if enabled, rules, personas, Mullvad account) encrypted
  at rest; key derived with argon2id from your password, cached in the kernel keyring.
  Chromium's own cookie store uses the OS secret service.
- Panic: one shortcut kills all sandboxes and wipes burners; optional full-profile wipe.
- Never expose `--remote-debugging-port`; DevTools only through CEF in-process.
- No crash dumps written to disk; pinned CEF versions with checksum verification.

---

## 5. Developer tools

- **Chrome DevTools**: the real frontend, docked in our own OSR pane (side/bottom/detached).
- **Wire** (our Wireshark-style panel):
  - sources: CDP `Network.*` (per request), Chromium NetLog (DNS, sockets, TLS handshakes,
    H2 frames, proxy tunnels), and our own per-sandbox proxy tap (raw connection bytes/timings).
  - TLS decryption in-app using Chromium's `SSLKEYLOGFILE` key log → see decrypted H1/H2 of
    any connection, including ones DevTools never shows (preconnects, service workers, extensions).
  - display-filter syntax (`host ~ "google" && tls.version == 1.3`), timeline waterfall,
    per-sandbox/persona view, blocked-tracker overlay, request replay/edit, pcapng export
    (opens in real Wireshark with keys).
- **Leak test** built in: IP, DNS, WebRTC, persona coherence, header consistency, per sandbox.

---

## 6. Design language — Nothing

- Pure black / white, one accent red (`#D71921`), greys only as hairlines.
- Type: JetBrains Mono (bundled, OFL) — the same face as the user's terminal and bar — rasterized
  with swash into a glyph atlas; installed Noto fonts as fallback for other scripts.
- Status shown as dots and dot rings (loading, VPN, blocked count), not icons.
- Lots of negative space, rounded cards, 1px strokes.
- **Motion**: spring physics everywhere (not fixed-duration easing), interruption-safe —
  a moving element retargets mid-flight keeping its velocity, so nothing ever "jumps".
  Render on demand at the display's refresh rate (120 Hz aware), nothing ever blocks the
  UI thread (CEF runs on its own pump, pages arrive as textures).

---

## 7. Phases

| phase | outcome |
|---|---|
| **P0 foundations** | workspace; `ui` core (renderer, springs, dot font); Nothing shell with animated tabs. Spikes: CEF OSR via DMA-BUF on Intel/Wayland, extensions in Alloy-style, per-context proxy. |
| **P1 usable browser** | ✅ zero-copy page frames (DMA-BUF → wgpu), tabs, omnibox, navigation, mouse/keyboard/wheel, popups → tabs. Left: IME, body text font, context menus, dialogs, downloads. Was: | page textures in the shell, input forwarding (mouse/keys/IME/scroll/touchpad), body-text rendering + omnibox editing, navigation, popups, our own context menus/dialogs, downloads, HiDPI. |
| **P2 isolation + shield** | ✅ shield (blocker, YouTube, anti-adblock, URL cleaning, panel, attempt log). Next: vault + PIN + history wipe, site inspector, profiles, per-site sandboxes, persona v1, referrer policy, permissions, burner tabs, rules. |
| **P3 tunnel** | Mullvad daemon integration, kill switch, per-sandbox SOCKS exits, DoH, WebRTC lockdown, vault + account number. |
| **P4 extensions** | install from Chrome Web Store (fetched through Mullvad), extension actions/popups in our UI, per-sandbox extension policy. |
| **P5 devtools + Wire** | docked DevTools, Wire panel, TLS keylog decryption, pcapng export, leak test. |
| **P6 hardening** | own CEF build with privacy patches (ungoogled-chromium set + deep fingerprint farbling), strict mode, fuzzing our IPC, update channel. ~100 GB disk, first build takes hours on this laptop. |
| **P7 extras** | command palette, vertical tabs/spaces, split view, overview grid, reader mode, Tor/arti, browser-only WireGuard, session snapshots, profile lock. |

## 8. Running

`scripts/install.sh` — release build installed as a desktop app ("unskilled browser" in the
launcher). Each install is `~/.local/lib/unskilled-browser/versions/<id>/` (CEF stripped — the
download ships a 1.4 GB unstripped libcef.so — and hard-linked between versions) with an `$ORIGIN`
rpath; `current` flips atomically. Chromium re-executes its own binary for helper processes, so a
running browser keeps its version directory until it exits; the bar then shows "update · restart",
which saves the vault and execs the new build. Old versions are removed once unused.
`scripts/auto-install.sh` (meant for an editor/agent hook) reinstalls in the background whenever
sources changed since the last install; log: `~/.cache/unskilled-browser/install.log`.
`scripts/package.sh` builds the portable release bundle (`dist/`, Linux tar.gz / Windows zip);
`.github/workflows/release.yml` runs it for both platforms on a `v*` tag and publishes a release.
`scripts/run.sh [--release]` is for development (rebuilds every run).
Window: no frame of its own (niri `prefer-no-csd`) — the 6 px edges resize, empty bar space drags,
double-click there maximizes. App id `unskilled-browser`.
Data: `~/.local/share/unskilled-browser` (vault + public filter lists), session in RAM at
`$XDG_RUNTIME_DIR/unskilled-browser`.
Dev hooks: `UNSKILLED_OPEN="url …"`, `UNSKILLED_PANEL=1`, `UNSKILLED_CHROMIUM_FLAGS="a=b c"`,
`UNSKILLED_SOFTWARE_PAINT=1`, `UNSKILLED_OZONE=x11|headless`, `UNSKILLED_INSPECTOR=1`,
`UNSKILLED_DEVTOOLS=1`, and in debug builds only `UNSKILLED_DEV_AUTORESTART` (restart as soon
as an update is detected), `UNSKILLED_DEV_PROMPT=auth|save` (raises a prompt sheet) and
`UNSKILLED_DEV_PIN` (types the PIN; use with throwaway `XDG_DATA_HOME`/`XDG_RUNTIME_DIR`).
TPM PIN mode needs `/dev/tpmrm0` access: `sudo usermod -aG tss $USER`, then log in again. A vault
created with a passphrase is offered a switch to a PIN after the next unlock once the TPM is usable.

Rendering notes (Intel Iris Xe, niri/Wayland): Chromium uses ozone `x11` (XWayland) purely for GPU
buffer allocation — no X windows are created — and ANGLE on Vulkan, whose DMA-BUF writes are
fenced (ANGLE/GL handed over half-drawn frames). Hardware video decode is off: its NV12 shared
images crash the GPU process in windowless mode.

## 9. Requirements on this machine

- Rust 1.99 (present in `~/.cargo/bin`).
- `cmake`, `ninja` — needed to build CEF's C++ wrapper: `sudo pacman -S --needed cmake ninja`.
- Vulkan (present: mesa 26.2, Intel Iris Xe) — needed for DMA-BUF import.
- CEF binary distribution (~1 GB unpacked) — downloaded automatically by the `cef` build script.

## 10. Profile features (shipped in 0.1)

- `crates/profile`: one JSON file (`profile.json`) in the vault's settings dir — prefs, bookmarks,
  passwords, never-save sites, sign-in decisions. Encrypted at rest with the rest of the vault.
- Internal pages `unskilled://settings` and `unskilled://bookmarks` are served from the binary
  (assets same-origin under each page, CSP `'self'`); a frozen `window.unskilled.call` bridge is
  injected only into `unskilled://` frames and the browser re-checks the frame's origin per call.
- Sign-in gate: navigations from site A to a provider's sign-in endpoint pause (`CONTINUE_ASYNC`)
  until the user answers the sheet; embedded provider frames never get the provider's cookies
  unless the site was allowed. FedCM is disabled so it can't bypass the gate.
- Passwords: `forms.js` reports login forms and submits; the shell offers to save, and the key in
  the address bar fills. Every fill/show/copy asks for the vault secret (`Vault::verify`) unless
  turned off — and turning it off is itself PIN-guarded. Copies clear from the clipboard after 30 s.
- Prompts are native sheets under the address bar (`shell/src/sheet.rs`).
- Omnibox loading: dots light along the pill's outline (top and bottom at once) behind red heads,
  spring-smoothed progress with a creep so it never stalls; the ring fades on completion.
- Windows: DX12 + D3D11 shared-texture import, passphrase-only vault, session under
  `%LOCALAPPDATA%` (no tmpfs). CI-built only. Chromium sandbox through CEF's prebuilt
  `bootstrap.exe` (since M138 the only way outside a Chromium build): the bundle ships it as
  `unskilled.exe`, icon and version info swapped by `crates/winmain/build.rs`, and it loads the
  browser from `unskilled.dll` (`crates/winmain`, exporting `RunWinMain`), passing the sandbox
  info on to `CefExecuteProcess`/`CefInitialize`. `cargo run` stays an unsandboxed plain exe.

## 11. Per-site sandboxes (shipped after 0.1)

- `engine/src/sandbox.rs`: one `RequestContext` per `profile::sandbox_of(url)` (registrable
  domain; a provider's owner sites grouped under its first owner), cache path
  `profile/sandbox-<site>` — Chromium's Chrome runtime refuses profiles that aren't direct
  children of the data dir ("Cannot create profile at path"). Contexts are created lazily; their
  profiles initialize asynchronously, so sandboxed browsers use `browser_host_create_browser`
  (async) and navigate from `on_after_created`.
- A main-frame GET to another sandbox is cancelled in `on_before_browse` and `TabState::swap` asks
  the shell for a fresh page there (`Shell::move_to_sandbox`); the shell keeps back/forward stacks
  across swaps. POSTs, popups (need `window.opener`) and provider sign-ins the site isn't allowed
  to use stay put. `unskilled://` pages always load in the global context.
- Sign-in sharing (`Tab::answer_identity(true)`) copies the provider's cookies from its sandbox into
  the site's (`sandbox::share_sign_in`); the gate still controls when they're sent.
- Vault: `profile/sandbox-<site>/…` follows the same allowlist as `profile/Default/…`; "forget"
  deletes cookies now and drops a `sandbox-<site>.forget` marker that `sandbox::init` acts on.
- Next: personas per sandbox (CDP `Emulation.*` overrides + canvas/audio/WebGL farbling), per-sandbox
  Mullvad SOCKS exits (`proxy` preference per context), privacy receipt panel, Wire panel.
