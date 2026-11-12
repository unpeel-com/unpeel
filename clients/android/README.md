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

```sh
clients/android/gradlew -p clients/android :protocol:test :app:assembleDebug
```

Debug APK: `clients/android/app/build/outputs/apk/debug/app-debug.apk`.
If a user-level Gradle init script redirects Maven to an unreachable mirror,
run that command with a clean `GRADLE_USER_HOME`.
