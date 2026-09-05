# unpeel-native-bridge

Panic-contained C ABI over `unpeel-core` for the Swift Mac app
(`apps/native`). The app links this static library to call backend logic
in-process (state reads/writes, session operations) instead of shelling out
for everything; every entry point catches Rust panics at the FFI boundary so
a backend bug can't take down the app.

Keep this crate a thin translation layer: logic belongs in `unpeel-core`, and
anything the TUI also needs must live there (see
`docs/plans/shared-core.md` — one core, two UIs).
