# Unpeel for Android

A remote Controller for an Unpeel Host, matching the iOS app's role: it never
hosts a session. Pair over the same `/mobile` protocol (`UNPEEL:` codes or the
JSON payload), then list sessions, stream the live terminal, send input, answer
approvals, create sessions from Host presets, and open the screenshot gallery.
Off-LAN traffic uses the Link relay with the same forward-secret handshake as
Swift and Rust (`protocol/relay-kat-vectors-v1.json`).

Not in this client yet: push notifications (the Host route is APNs-shaped),
voice dictation, image annotation, and a Ghostty-grade terminal. The terminal
here is a VT parser sized to the Host grid; keyboard focus does not resize
that grid.

This tree is the Android client from `native/ui-localization`, with the
vendored Termux sources replaced by the MIT `:termit` module.

```sh
clients/android/gradlew -p clients/android :protocol:test :app:assembleRelease
```

Release APK: `clients/android/app/build/outputs/apk/release/app-release.apk`.
If a user-level Gradle init script redirects Maven to an unreachable mirror,
run that command with a clean `GRADLE_USER_HOME`.

CI (Actions → **Android APK**) builds the **release** APK and attaches it as a
workflow artifact — no GitHub Release is created. Optional signing via repo
secrets `ANDROID_KEYSTORE_BASE64` / `ANDROID_KEYSTORE_PASSWORD` /
`ANDROID_KEY_ALIAS` / `ANDROID_KEY_PASSWORD`; otherwise the release build is
signed with the debug key so it still sideloads. Trigger with
`workflow_dispatch`, or push to `android/termit` under `clients/android/`.
