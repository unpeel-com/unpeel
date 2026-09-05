# Unpeel for iPhone / iPad

The iOS app — a **remote controller** for Unpeel Hosts (Mac app or headless
TUI), live on TestFlight (`unpeel.com/ios`). It pairs with a Host (QR/paste),
then lists sessions, streams live terminals, sends input/verbs, and receives
attention push notifications. Off-LAN traffic rides the Link relay with a
forward-secret E2E handshake; the relay never sees content.

Product framing (see AGENTS.md): fleet glanceability + attention
notifications + control verbs rank above terminal polish, and the detail
screen stays a live terminal — never a semantic chat UI. Two hard terminal
rules: keyboard focus must never resize the remote grid, and agent-drawn
select menus are detected from rendered viewport text (they fire no hooks).

## Layout

- `UnpeelIOS/` — the Xcode project (SwiftUI app, Ghostty-based terminal
  rendering, `RemotePreviewStore` for previews — mock data is preview-only,
  never runtime)
- `UnpeelIOS/Tools/dev_bridge.py` — dev bridge for driving the simulator
  against a local desktop instance
- `PRD.md` — original product framing

Shared protocol/crypto (pairing, remote control, Relay E2E) lives in
`apps/shared/UnpeelShared`, not here.

## Build & test

Built/deployed via `xcodebuild` (see `docs/agents/dev-builds.md` for signing
and device gotchas). Package tests: `swift test` is broken on macOS for this
package — use the xcodebuild package-scheme recipe in that doc.
