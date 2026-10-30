# Contributing

Thanks for your interest!

**Bugs and features**
- Open an issue first for anything bigger than a small fix, so we can agree on the approach.
- Security issues go through [SECURITY.md](SECURITY.md), not the issue tracker.

**Building**
- See the README.
- `scripts/run.sh` runs a debug build straight from the source tree.

**Before sending a pull request**
- `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
- `cargo test --workspace` must pass.
- Keep changes focused, and match the style of the surrounding code: comment density, naming and idiom.

**Design rules**
- No UI frameworks. The interface is drawn by `crates/ui`.
- Pure black and white, one red accent, greys only as hairlines, JetBrains Mono.
- Every animation is a spring, never a fixed-duration tween.
- Nothing may contact a server the user didn't ask for. That rules out telemetry, update pings and remote fonts on internal pages.

**Licensing**
- Contributions are accepted under the project's license, GPL-3.0-or-later.
