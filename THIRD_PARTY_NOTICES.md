# Third-party notices

unskilled browser is licensed under the GNU General Public License v3.0 or later (see `LICENSE`).
It includes or downloads the following third-party work.

## Shipped with the browser

| component | license | notes |
|---|---|---|
| [Chromium](https://www.chromium.org/) via the [Chromium Embedded Framework](https://bitbucket.org/chromiumembedded/cef) | BSD-3-Clause and others | the full list of Chromium's third-party licenses is in `CHROMIUM_CREDITS.html` in every release bundle |
| [uBlock Origin](https://github.com/gorhill/uBlock) scriptlets and redirect resources | GPL-3.0 | extracted into `crates/shield/assets/resources.json` by `crates/shield/src/bin/ubo-resources.rs` |
| [JetBrains Mono](https://www.jetbrains.com/lp/mono/) | SIL Open Font License 1.1 | `crates/ui/assets/fonts/OFL.txt` |
| [adblock-rust](https://github.com/brave/adblock-rust) | MPL-2.0 | filter engine |
| [cef-rs](https://github.com/tauri-apps/cef-rs) | Apache-2.0 / MIT | CEF bindings |
| [wgpu](https://github.com/gfx-rs/wgpu), [winit](https://github.com/rust-windowing/winit), [swash](https://github.com/dfrg/swash) | Apache-2.0 / MIT | rendering, windowing, text |
| [tss-esapi](https://github.com/parallaxsecond/rust-tss-esapi) | Apache-2.0 | TPM 2.0 (Linux) |
| RustCrypto [argon2](https://github.com/RustCrypto/password-hashes), [chacha20poly1305](https://github.com/RustCrypto/AEADs) | Apache-2.0 / MIT | vault cryptography |

Every Rust dependency and its license is listed in `Cargo.lock`. To print them, run `cargo install cargo-about && cargo about generate about.hbs`, or `cargo license`.

## Downloaded at runtime (never bundled)

| data | source | license |
|---|---|---|
| uBlock Origin filter lists (filters, badware, privacy, quick-fixes, unbreak, removeparam) | uBlockOrigin/uAssets | GPL-3.0 |
| EasyList, EasyPrivacy, Adblock Warning Removal List | easylist.to | GPL-3.0 / CC BY-SA 3.0 |
| Peter Lowe's ad server list | pgl.yoyo.org | McRae General Public License |
| Online Malicious URL Blocklist | malware-filter (curbengh) | MIT / CC0 |
| IP to ASN database (site inspector) | [iptoasn.com](https://iptoasn.com) | PDDL v1.0 |
