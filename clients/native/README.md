# Unpeel Native

The macOS app — Unpeel's primary client. Swift + SwiftUI embedding libghostty
(GhosttyKit, Metal) terminal surfaces. The app is a Controller of the bundled
`unpeel serve` Host service plus a platform-capability adapter (notifications,
Keychain, APNs, approval dialogs); the session backend it drives is the
`crates/` Rust workspace in this same tree, so app and server can never skew.

Layout:

- `UnpeelNative/` — the Swift macOS app: sidebar/session UI
  (`UnpeelStore.swift`, the Controller projection), the loopback listener
  (`HookServer.swift`, platform-adapter callback only — no Host routes), the
  startup activity seed (`SessionActivity.swift`), binary resolution
  (`LaunchConfig.swift`), licensing (`Licensing/LicenseManager.swift`), and
  the remote Host runtime (`RemoteHostRuntime.swift`).
- `UnpeelNative/Sources/CUnpeelNativeBridge/` — the C shim over
  `crates/unpeel-native-bridge` (the header must stay identical to the
  crate's `include/unpeel_native_bridge.h`; `build-rust-bridge.sh` checks).
- `vendor/libghostty-spm/` — vendored libghostty Swift package with local
  patches (see `UNPEEL-PATCHES.md` there); every bump is a deliberate event.
- `build-rust-bridge.sh [debug|release]` — builds the bridge static library
  into `crates/target/native-bridge/<profile>/`, which `Package.swift` links.
- `build-app.sh` — assembles `dist/Unpeel.app`: the release app, the bridge,
  and `unpeel-host` + `unpeel` + `unpeel-attach` built **from this tree**
  (`UNPEEL_SERVER_ARCHIVE=<tar.gz>` bundles a published CLI archive of the
  same version instead, for reproducibility checks), Sparkle, notices, icon,
  Info.plist, then code-signs (ad-hoc by default).
- `dev-app.sh` / `dev-blank.sh` (`bun run dev:native` / `dev:native:blank`)
  — dev builds with a stable signing identity; they show **"Unpeel Dev"**
  with a burnt-orange icon. `dev-blank.sh` runs against a throwaway
  `UNPEEL_HOME`.
- `release.sh` (`bun run release:mac`) — the full release pipeline: build +
  Developer ID sign → notarize + staple → DMG → Sparkle ZIP + appcast →
  publish to R2. Needs the operator's local secrets; `--dry-run` rehearses.
- `test-native.sh` — debug bridge + `swift test`. `rehearse-upgrade.sh` —
  Sparkle upgrade rehearsal. `verify-attach.sh` / `verify-browser.sh` — shims
  to the server-side smoke tests in `scripts/`.
- `release-notes/` — historical Sparkle release-note HTML.

Hard rules (the root `AGENTS.md` is authoritative): never write to, launch,
or quit `/Applications/Unpeel.app`; develop against `dist/Unpeel.app` and
check the menu bar says "Unpeel Dev". Ghostty surfaces cannot initialize in
headless agent runs — verify Metal rendering interactively only.

Build: `clients/native/build-rust-bridge.sh debug` then `swift build` in
`UnpeelNative/` for a compile check; `bun run dev:native` from the repo root
for a runnable signed app; `CODESIGN_IDENTITY=- clients/native/build-app.sh`
for an unsigned (ad-hoc) bundle without launching anything.
