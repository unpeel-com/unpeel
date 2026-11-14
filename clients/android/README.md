# Unpeel for Android

A remote Controller for an Unpeel Host, matching the iOS app's role: it never
hosts a session. Pair over the same `/mobile` protocol (`UNPEEL:` codes or the
JSON payload), then list sessions, stream the live terminal, send input, answer
approvals, create sessions from Host presets, and open the screenshot gallery
(including crop / arrow markup). Off-LAN traffic uses the Link relay with the
same forward-secret handshake as Swift and Rust
(`protocol/relay-kat-vectors-v1.json`).

One phone keeps multiple paired Hosts; each pairing is one workspace. The
Workspaces sheet switches the active pairing locally — it does not call a Host
cross-workspace RPC.

Not in this client yet: push notifications (the Host route is APNs-shaped),
voice dictation, full-screen ink annotation, and a Ghostty-grade terminal. The
terminal here is a VT parser sized to the Host grid; keyboard focus does not
resize that grid.

Runtime presentation (agent vs shell, transcript offer, spinner tint) comes
from `GeneratedRuntimeCatalog.kt`, regenerated with `bun run generate:runtimes`
from `runtimes/*/runtime.toml` (same source as the Swift catalog).

```sh
clients/android/gradlew -p clients/android :protocol:test :app:assembleRelease
```

Release APK: `clients/android/app/build/outputs/apk/release/app-release.apk`.

Versioning: `versionName` is read from `[workspace.package] version` in
`crates/Cargo.toml` at build time, so a release bumps it there and nowhere
else. `versionCode` is the Android build number, a separate monotonic space
(like the Mac app's `CFBundleVersion`); raise it in `app/build.gradle.kts` for
every APK that is published.
If a user-level Gradle init script redirects Maven to an unreachable mirror,
run that command with a clean `GRADLE_USER_HOME`.

CI (Actions → **Android APK**) builds the **release** APK and attaches it as a
workflow artifact — no GitHub Release is created. Optional signing via repo
secrets `ANDROID_KEYSTORE_BASE64` / `ANDROID_KEYSTORE_PASSWORD` /
`ANDROID_KEY_ALIAS` / `ANDROID_KEY_PASSWORD`; otherwise the release build is
signed with the debug key so it still sideloads. Trigger with
`workflow_dispatch`, or push to `android/termit` under `clients/android/`.
