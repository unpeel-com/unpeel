# Unpeel iOS Remote Controller PRD

Last updated: 2026-06-22

## 1. Summary

Unpeel iOS is a native iPhone and iPad companion for controlling Unpeel sessions hosted by the Mac app. It is not a local terminal host and it does not run agent CLIs on iOS. The Mac remains the owner of PTYs, provider wrappers, hook events, session manifests, project state, presets, worktrees, and terminal control sockets.

The iOS app connects to a paired Mac, shows live sessions grouped by project/folder, lets users create sessions from existing Mac presets, write prompts into sessions, send key commands, respond to attention states, and organize the workspace. It should feel like a focused control surface for visual workers who want to steer agents without sitting at the Mac.

The first implementation should establish a shared Swift module and a remote-control protocol that can be used by both the Mac bridge and iOS client. The iOS app should be useful on local network first, then leave room for relay and notifications later.

## 2. Product Decision

The iOS app is remote-controller only.

This has several consequences:

- iOS never launches `claude`, `codex`, `amp`, `gemini`, `opencode`, `pi`, `grok`, `cursor-agent`, `unpeel-host`, or `unpeel-attach` locally.
- iOS never opens the Mac user's `~/.unpeel` directory directly.
- iOS never speaks to `session.sock` directly.
- iOS uses a dedicated authenticated Mac bridge, not the existing Unpeel Sessions MCP token.
- All actions that can execute code happen on the Mac after auth, policy checks, and audit logging.
- The Mac app is the authority for session creation and workspace organization.

## 3. Existing Architecture Context

Current Mac architecture:

- Hosted PTY sessions live in `unpeel-host` processes.
- Session artifacts live under `~/.unpeel/app-sessions/<session-id>/`.
- `manifest.json` stores session metadata and running/exited state.
- `output.bin` stores a bounded sparse terminal journal: logical offsets remain
  append-only, while old physical blocks are reclaimed and stale cursors rebase
  to a retained tail.
- `session.sock` receives writes, resize, ping, and kill commands while the host is alive.
- The native Mac app owns spawning, sidebar state, activity state, project overlays, preset overlays, title overrides, and settings UI.
- The native Mac app already exposes a local hook HTTP server and `/mcp/*` bridge for host-side MCP lifecycle actions.

iOS should reuse these primitives through a new bridge layer:

- Read session/project/preset state from the Mac app.
- Subscribe to activity and terminal viewport updates from the Mac app.
- Ask the Mac app to start/close/restart sessions.
- Ask the Mac app to write text or keys into sessions through the same safe paths as MCP `send_text` and `send_keys`.
- Ask the Mac app to mutate workspace organization through the same native state writers used by the sidebar/settings UI.

## 4. Goals

- Pair an iOS device with a Mac intentionally and securely.
- List live and saved sessions grouped by project and project folder.
- Show activity, unread, pinned, attention, provider, project, and worktree state.
- Open a session and render a readable terminal viewport.
- Send prompts into a session with paste-safe semantics.
- Send key commands for terminal interaction, including Enter, Esc, Tab, arrows, and Ctrl-C.
- Create new sessions in an existing Mac project or worktree using Mac presets.
- Optionally send an initial prompt immediately after session creation.
- Rename, pin, archive/remove, and restart sessions where the Mac app supports those actions.
- Create, rename, reorder, and color project folders.
- Move projects into/out of folders.
- Respect existing blocked-project and remote-control security policy.
- Keep the shared protocol/data models usable by Mac, iOS, and tests.

## 5. Non-Goals

- Running local terminals or agent CLIs on iOS.
- Building an SSH client, Mosh client, or generic mobile terminal app.
- Replacing the Mac app as the source of truth.
- Directly exposing raw Unix sockets, manifest files, app-state files, or MCP auth over the network.
- Supporting arbitrary Mac filesystem browsing from iOS in V1.
- Supporting cloud relay in V1.
- Supporting unpaired browser clients in V1.
- Allowing background remote execution without a paired Mac policy.
- Implementing full session transcript search in V1.

## 6. Target Users

### Designer Steering Agents

Runs Claude/Codex sessions while designing or reviewing UI. Wants to check status, send follow-up prompts, and answer approval prompts from the phone.

### Front-End Developer Away From Desk

Starts multiple sessions before stepping away. Wants to see which sessions are busy, stuck, or finished and nudge them without returning to the Mac.

### Multi-Agent Orchestrator User

Runs several agents across worktrees. Wants the phone to be a session inbox with quick controls, not a full terminal-first interface.

### iPad Desk Companion User

Keeps an iPad next to the Mac as a persistent session board. Wants denser navigation, split view, and keyboard support over time.

## 7. Product Principles

- Remote control is powerful and dangerous. Make access explicit, visible, revocable, and logged.
- The iOS home screen is a session inbox first, terminal second.
- Attention states should be surfaced before raw terminal detail.
- Writing into sessions should be ergonomic, but clearly tied to the selected Mac and session.
- Terminal rendering should prioritize legibility, stable layout, and correct interaction over decoration.
- iOS should use Mac presets and project state rather than inventing separate iOS configuration.
- Shared code should be protocol and product-model first; platform UI and process control remain separate.

## 8. Primary User Journeys

### 8.1 Pair iPhone With Mac

1. User opens Settings > Remote Control on Mac.
2. Mac shows "Pair iPhone" and a QR code.
3. iPhone scans QR code.
4. iPhone asks for Local Network permission if needed.
5. iPhone connects to the Mac bridge using the one-time pairing token.
6. Mac records the device and shows it as paired.
7. iPhone lands on the session inbox.

Acceptance:

- Pairing completes in under 30 seconds on the same network.
- Mac shows paired device name and last-seen time.
- User can revoke the device from Mac settings.

### 8.2 Check Session Status

1. User opens iOS app.
2. App reconnects to last paired Mac.
3. Home shows project/folder groups and active sessions.
4. Attention sessions appear at the top.
5. User taps a session to inspect terminal state.

Acceptance:

- Busy, idle, attention, unread, pinned, and exited state are visually distinct.
- Stale/disconnected state is clear.
- Session list updates without manual refresh while the app is foregrounded.

### 8.3 Write Into Existing Session

1. User opens a session.
2. Terminal viewport renders current screen.
3. User types a prompt in the composer.
4. User taps Send.
5. Mac bridge writes using bracketed paste plus submit behavior.
6. Terminal updates stream back.

Acceptance:

- Long pasted prompts do not get split into unintended shell commands.
- Send action shows immediate feedback.
- App blocks sending if session is exited, unreachable, or write policy forbids it.

### 8.4 Answer Interactive Menu

1. Session is waiting on a menu or approval prompt.
2. User sees attention indicator.
3. User opens the session.
4. User uses quick keys: arrows, Enter, Esc, Ctrl-C.
5. Mac bridge sends key events with pacing.

Acceptance:

- Arrow/Enter menu handling works for common TUIs.
- Ctrl-C requires an intentional confirmation or long press.
- Key strip is reachable without covering terminal content.

### 8.5 Create Session From iOS

1. User taps New Session.
2. User chooses project or worktree.
3. User chooses preset/tool, defaulting to the Mac's default preset for that CLI.
4. User optionally enters an initial prompt.
5. Mac app spawns the session through the existing native spawn path.
6. iOS selects the new session and shows its terminal once attached.

Acceptance:

- New sessions appear in Mac sidebar and iOS inbox.
- Preset resolution matches Mac behavior.
- Blocked projects refuse creation with a clear error.
- Initial prompt sends only after the host is attachable.

### 8.6 Organize Folder+

1. User opens Projects tab or edit mode from Home.
2. User creates a project folder.
3. User renames folder, chooses color, and reorders it.
4. User moves projects into the folder.
5. Mac sidebar reflects organization after sync.

Acceptance:

- iOS can create, rename, recolor, reorder, and delete empty folders.
- iOS can move existing projects into/out of folders.
- Worktree child projects inherit parent folder context unless explicitly represented otherwise.
- Mutations are serialized through the Mac app to avoid app-state conflicts.

## 9. Information Architecture

### iPhone V1

Primary tabs:

- Home: session inbox grouped by folders/projects.
- Projects: project and folder organization, new session by project.
- Settings: paired Mac, connection, device info, security, about.

Session screen:

- Title bar: session title, project/worktree, activity, connection state.
- Terminal viewport: scrollable rendered screen and optional tail.
- Composer: text input, send, key strip, paste mode.
- More menu: rename, pin/unpin, restart, close, archive/remove, copy session id.

New session flow:

- Project picker.
- Preset picker.
- Optional initial prompt.
- Create.

### iPad Later

Use a split layout:

- Sidebar: folders/projects/session inbox.
- Detail: terminal/session screen.
- Inspector or bottom panel: composer, keys, metadata.

Keyboard support should include hardware keyboard shortcuts for send, escape, tab, arrows, session switch, and search when those features exist.

## 10. Functional Requirements

### 10.1 Pairing and Device Management

Mac requirements:

- Remote control is off by default.
- User can enable remote control in Settings.
- User can create a short-lived pairing QR code.
- User can view paired devices.
- User can revoke devices.
- User can see currently connected devices.
- User can disable all remote control immediately.

iOS requirements:

- Scan QR code.
- Store paired Mac credentials in Keychain.
- Show paired Mac name and connection state.
- Forget Mac.
- Re-pair if credentials are revoked or expired.

### 10.2 Session List

The Mac bridge must provide:

- Session id.
- Title.
- Command.
- Provider/tool id.
- Project id.
- Worktree path/branch.
- Running/exited status.
- Busy/idle/attention state.
- Unread state.
- Pinned state.
- Created and updated timestamps.
- Last output preview if safe and available.

Sorting:

- Attention first.
- Busy next.
- Pinned next.
- Recent activity next.
- Exited sessions lower unless pinned.

### 10.3 Project and Folder Organization

The Mac bridge must provide:

- Project summaries.
- Folder summaries.
- Parent/child relationships.
- Sort order.
- Color id.
- Worktree metadata.
- Blocked remote/MCP state.

Mutations:

- Create folder.
- Rename folder.
- Recolor folder.
- Reorder folder.
- Delete empty folder.
- Move project to folder.
- Rename project display name if supported by Mac overlay.
- Reorder projects.

Constraints:

- iOS should not write `~/.unpeel/app-state.json` directly.
- Mac app performs all read-modify-write operations.
- If Mac has unsaved or conflicting state, Mac wins and iOS receives the resolved state.

### 10.4 Session Creation

Supported V1 inputs:

- Existing project id.
- Optional existing worktree path/branch.
- Preset id or explicit command if policy allows.
- Optional initial prompt.
- Submit mode: paste only, paste and submit, raw.

Policy:

- V1 should prefer preset-based creation.
- Arbitrary command creation should be disabled by default for iOS unless explicitly enabled.
- Blocked projects refuse remote creation.
- The Mac app owns spawned session registration, sidebar update, hook env, and manifest polling.

### 10.5 Session Input

Text input modes:

- `pasteAndSubmit`: bracketed paste, settle, Enter.
- `pasteOnly`: bracketed paste without Enter.
- `raw`: raw write, reserved for advanced/debug.

Key input:

- Enter.
- Escape.
- Tab.
- Arrow up/down/left/right.
- Ctrl-C.
- Ctrl-D.
- Ctrl-Z.

Safety:

- Ctrl-C, Ctrl-D, and Ctrl-Z should require confirmation or a deliberate gesture.
- Input to the calling/selected session is allowed because iOS is not itself a hosted agent session.
- Input to exited sessions is rejected.
- Input to sessions in blocked projects is rejected if remote policy says blocked.

### 10.6 Terminal Viewport

V1 should render structured viewport frames, not raw ANSI as the primary app contract.

Frame fields:

- Session id.
- Sequence number.
- Rows and columns.
- Cell text.
- Foreground/background color.
- Bold, italic, underline, inverse, dim, strikethrough.
- Cursor row/column/shape/visibility.
- Alternate-screen flag.
- Capture timestamp.

Performance path:

- Start with full viewport snapshots.
- Add dirty-region diffs if needed.
- Use scrollback tail as a separate read model.

### 10.6.1 Semantic Conversation View

For agent providers with durable transcript/session storage, iOS should prefer a
native conversation view over the terminal viewport. The terminal remains the
fallback and the write/key transport.

See `docs/feature/remote-transcript-api.md` for the provider matrix and shared
adapter implementation.

V1 candidate providers:

- Claude and Codex, because Unpeel already resolves and parses their transcripts.
- Cursor Agent, after adding the parser/trust-root adapter.

Follow-on providers:

- Gemini CLI, after fresh launch verification of provider session capture.
- OpenCode, using its own session database adapter rather than Claude/Codex logs.
- Grok, after fresh launch verification of `GROK_SESSION_ID` capture.

Requirements:

- The session detail screen chooses chat view only when the Mac bridge reports a
  resolved transcript source.
- Unsupported or unresolved sessions open in terminal mode.
- Users can switch to terminal mode from chat mode for raw TUI state, approvals,
  login/setup screens, and debugging.
- Sending text and keys still routes through the Mac bridge and hosted session,
  not through provider transcript files.

### 10.7 Attention and Provider Events

iOS should show attention state from the Mac activity engine.

V1:

- Show attention cards and badges.
- Let user open session and use text/keys.
- Show provider-specific text where available.

Later:

- Provider-aware approve/deny buttons.
- Push notifications for attention.
- Notification actions for simple approve/deny where safe.

### 10.8 Session Organization

Actions:

- Rename session.
- Pin/unpin.
- Archive/remove from visible list if supported.
- Restart session.
- Close/kill session.

Safety:

- Restart and close require confirmation.
- Kill/close should use existing Mac cleanup path.
- Restart should preserve resume semantics through `ResumeCommand` on Mac.

## 11. Shared Code Refactor

The Swift codebase should move toward these target boundaries:

```text
apps/shared/UnpeelShared/
  Sources/
    UnpeelShared/
      Cross-platform data models
      Remote-control protocol types
      Pure parsing helpers
      License payload/signature helpers where portable
```

Mac target:

```text
apps/native/UnpeelNative/
  Package.swift depends on ../../shared/UnpeelShared
  Sources/
    UnpeelNative/
      Mac app shell
      AppKit/Ghostty/Sparkle integration
      Local session spawning
      Mac remote-control bridge
      Mac settings/sidebar UI
```

Future iOS target:

```text
apps/ios/UnpeelIOS/
  Sources/
    UnpeelIOS/
      SwiftUI app shell
      Remote client
      Pairing flow
      Session inbox
      Terminal viewport renderer
      Project/folder organizer
```

Rules:

- `UnpeelShared` must not import AppKit, UIKit, SwiftUI, Ghostty, Sparkle, or Security UI.
- `UnpeelShared` can import Foundation.
- Platform targets adapt shared models into platform UI.
- Mac-only process and filesystem behavior stays out of `UnpeelShared`.
- Protocol types should be Codable, Equatable, and Sendable where practical.

Initial shared candidates:

- Remote-control protocol models.
- Session summary DTOs.
- Project/folder DTOs.
- Preset DTOs.
- Terminal viewport DTOs.
- Pure command parsing helpers after auditing current dirty changes.
- License payload structs, separated from Keychain and URLSession behavior.

## 12. Mac Remote Bridge Architecture

The Mac bridge should live in the native Mac app, not only in `unpeel-host`, because session creation, project organization, presets, and sidebar state are native-app-owned.

Responsibilities:

- Own pairing and paired-device registry.
- Start/stop the remote server when enabled.
- Authenticate every request.
- Authorize read/write actions per device and project.
- Marshal state from `UnpeelStore`.
- Call existing session spawn, restart, close, and organization methods.
- Request viewport snapshots from session hosts.
- Write input through existing control socket helpers.
- Broadcast updates to connected iOS clients.
- Audit sensitive actions.

The bridge should remain separate from:

- Hook POST routes.
- `/mcp/*` routes.
- MCP auth token.
- Raw `session.sock`.

## 13. Transport

Recommended V1:

- LAN only.
- Bonjour discovery after pairing if feasible.
- QR code contains local endpoint, pairing token, Mac id, protocol version, and certificate fingerprint if TLS is enabled.
- WebSocket for live state and viewport updates.
- HTTPS or WebSocket request/response messages for mutations.

Connection model:

- iOS connects when foregrounded.
- Mac sends initial snapshot.
- Mac streams deltas for sessions, projects, folders, activity, and subscribed viewport.
- iOS reconnects with exponential backoff.
- If Mac endpoint changes, iOS uses Bonjour or asks user to re-pair.

## 14. API Sketch

HTTP-style endpoints, exact transport can be WebSocket messages if simpler:

```text
POST /remote/pair/claim
GET  /remote/status
GET  /remote/bootstrap
GET  /remote/sessions
POST /remote/sessions
POST /remote/sessions/:id/text
POST /remote/sessions/:id/keys
POST /remote/sessions/:id/restart
POST /remote/sessions/:id/close
PATCH /remote/sessions/:id
GET  /remote/projects
POST /remote/folders
PATCH /remote/folders/:id
DELETE /remote/folders/:id
PATCH /remote/projects/:id/organization
GET  /remote/presets
GET  /remote/devices
DELETE /remote/devices/:id
```

WebSocket message families:

```text
hello
auth.challenge
auth.ready
bootstrap.snapshot
sessions.changed
projects.changed
folders.changed
activity.changed
viewport.subscribe
viewport.unsubscribe
viewport.frame
input.accepted
input.rejected
mutation.accepted
mutation.rejected
device.revoked
error
```

Every message should include:

- Protocol version.
- Message id.
- Timestamp.
- Type.
- Optional request id.
- Optional session id.
- Body.

## 15. Security Requirements

Remote control can read secrets and inject commands. Treat it as high impact.

V1 requirements:

- Remote control off by default.
- Pairing requires explicit Mac user action.
- Pairing token is short-lived and single-use.
- Paired device credential is device-specific.
- Credentials stored in iOS Keychain.
- Mac stores device credentials in Keychain or a protected `0600` file.
- Device revocation takes effect immediately.
- Connections require authentication after pairing.
- Failed auth attempts are rate limited.
- Sensitive actions are audit logged.
- Mac UI shows when remote control is enabled.
- Mac UI shows connected remote devices.
- Remote writes are rejectable by policy.
- Browser CSRF cannot trigger remote write endpoints.

Preferred crypto:

- TLS with certificate fingerprint pinned from QR, or
- Noise-style encrypted session with a QR-transferred pairing secret.

Prototype fallback:

- Local-network HTTP with high-entropy token may be acceptable only for internal development and must not ship as production default.

Audit events:

- Remote server enabled/disabled.
- Pairing token created.
- Device paired/revoked.
- Device connected/disconnected.
- Session created.
- Text/key input sent.
- Session restarted/closed.
- Project/folder organization changed.
- Auth failure.

## 16. Permissions and Policy

Remote policy should be separate from MCP grants.

Recommended model:

- Global remote-control enabled flag.
- Per-device read/write permission.
- Per-project remote blocked flag or reuse `mcp_blocked_projects` for V1 if we intentionally want one shared "external control blocked" switch.
- Optional "allow arbitrary command from remote" flag, default false.
- Optional "confirm destructive actions on Mac" flag.

Default V1 policy:

- Paired devices can read sessions.
- Paired devices can write to sessions unless project is blocked.
- Paired devices can create sessions from enabled presets.
- Paired devices cannot run arbitrary commands unless user enables it.
- Paired devices can organize folders/projects.
- Close/restart requires iOS confirmation.

## 17. Terminal Rendering Design

Renderer options:

### Option A: SwiftUI Text Grid

Pros:

- Fast to build.
- Easy dynamic type and selection experiments.

Cons:

- Hard to make performant for large grids.
- Wide-character and cell measurement risk.

### Option B: CoreText/CoreGraphics Custom View

Pros:

- Better control over cell layout.
- Good enough for V1 snapshots.
- Portable to iOS without Ghostty UI wrappers.

Cons:

- More custom drawing.
- Needs careful accessibility and selection work.

### Option C: Ghostty/libghostty iOS Renderer

Pros:

- The vendored package declares iOS support.
- Potentially best rendering fidelity.

Cons:

- Current Unpeel bridge is AppKit/NSView/exec-backend.
- iOS still cannot run local PTYs.
- Higher integration risk.

Recommendation:

- V1 should use a CoreText/CoreGraphics renderer over structured viewport frames.
- Keep the protocol renderer-agnostic so Ghostty iOS rendering can be explored later.

## 18. UX Requirements

Visual tone:

- Native SwiftUI.
- Dense but calm session inbox.
- Clear project/folder hierarchy.
- Minimal decoration around terminal content.
- Glass/chrome only where it does not reduce terminal readability.
- Respect system light/dark for app chrome.
- Terminal theme can follow Mac session style or iOS preference.

Controls:

- Icon buttons for send, keys, restart, close, pin, rename, and folder actions.
- Text labels for destructive confirmations.
- Haptics for send, attention open, pair success, and destructive confirm.
- Avoid hiding key commands behind long menus only.

Accessibility:

- VoiceOver labels for sessions, activity, and remote write actions.
- Dynamic Type for app chrome.
- Terminal font size control independent of Dynamic Type.
- High-contrast mode support.
- Reduced motion support.

## 19. Error States

Required states:

- No paired Mac.
- Pairing token expired.
- Local Network permission denied.
- Mac offline.
- Mac reachable but auth rejected.
- Device revoked.
- Protocol version mismatch.
- Remote control disabled on Mac.
- Project blocked.
- Session exited.
- Session missing/stale.
- Viewport unavailable from older host.
- Input write failed.
- Session creation failed.

Each error should tell the user what changed and the next useful action.

## 20. MVP Scope

V1 must ship:

- Shared Swift `UnpeelShared` protocol models.
- Mac remote-control setting and pairing QR.
- iOS pairing flow.
- One paired Mac at a time.
- Session inbox grouped by project/folder.
- Live activity/unread/attention updates.
- Session viewport from full snapshots.
- Text composer with paste-and-submit.
- Key strip: Enter, Esc, Tab, arrows, Ctrl-C.
- Create session from existing project and preset.
- Optional initial prompt on create.
- Rename and pin sessions.
- Create/rename/reorder/color folders.
- Move projects into folders.
- Device revoke from Mac.
- Basic audit log.

V1 can defer:

- Cloud relay.
- Push notifications.
- Multiple Macs connected at once.
- File transfer.
- Full scrollback search.
- Raw arbitrary command creation from iOS.
- Provider-specific approve/deny buttons.
- Viewport diffs if snapshots are acceptable.
- Apple Watch.

## 21. Milestones

### Milestone 0: Shared Contracts

- Add standalone `UnpeelShared` package.
- Add remote protocol DTOs and tests.
- Keep Mac target compiling with shared dependency.

### Milestone 1: Mac Bridge Prototype

- Add remote bridge server behind development flag.
- Expose bootstrap snapshot.
- Expose session list and project/folder list.
- Expose text/key input for localhost-only development.

### Milestone 2: iOS Shell With Mock Data

- Add iOS app target.
- Build pairing placeholder.
- Build session inbox.
- Build session detail and composer against mock shared models.
- Build project/folder organizer against mock shared models.

### Milestone 3: Pairing and Auth

- Add QR pairing on Mac.
- Add Keychain storage on iOS.
- Add paired device registry on Mac.
- Add authenticated connection.
- Add revoke.

### Milestone 4: Live Sessions

- Connect iOS session inbox to Mac.
- Stream activity updates.
- Stream viewport snapshots.
- Send text and keys.
- Create sessions from presets.

### Milestone 5: Organization

- Wire folder/project mutations through Mac state writers.
- Sync Mac sidebar after iOS changes.
- Add conflict handling.
- Add audit entries.

### Milestone 6: Hardening

- Rate limiting.
- TLS/fingerprint or encrypted session.
- Error states.
- Accessibility pass.
- Battery/performance pass.
- Provider matrix smoke test.

## 22. Test Plan

Shared module:

- Codable round trips for every protocol DTO.
- Backward-compatible decode tests when adding optional fields.
- Protocol version compatibility tests.

Mac bridge:

- Pair token expiry.
- Pair token single-use behavior.
- Auth rejection.
- Device revoke disconnects active client.
- Blocked project rejects read/write/create if policy says blocked.
- Session creation uses same Mac spawn path as sidebar/MCP.
- Text input uses bracketed paste semantics.
- Key input pacing works for menus.
- Folder/project mutations preserve unrelated app-state fields.

iOS:

- Pairing happy path.
- Local Network permission denied.
- Reconnect after Mac app restart.
- Session inbox sorting.
- Composer disabled for exited/unreachable sessions.
- Key strip sends correct keys.
- Folder create/rename/reorder/move flows.
- Dynamic Type and terminal font size.
- VoiceOver labels.

End-to-end provider smoke:

- Claude.
- Codex.
- Amp.
- Gemini.
- OpenCode.
- Grok.
- Plain shell.

## 23. Success Metrics

Activation:

- Paired device setup completes in under 30 seconds for 90 percent of successful attempts.
- Less than 5 percent pairing failure rate on same-network attempts after permission grant.

Usefulness:

- User can identify attention sessions from Home without opening terminal.
- User can create a preset-based session from iOS in under 15 seconds.
- User can send a follow-up prompt and observe response without returning to Mac.

Reliability:

- Reconnect succeeds after Mac app restart when bridge returns.
- Device revocation prevents further access immediately.
- No known path exposes raw local control sockets over the network.

Performance:

- Session inbox updates feel realtime under normal LAN conditions.
- Terminal viewport renders at interactive speed for common 80x24 and 120x40 frames.
- Foreground iOS app avoids excessive battery drain during idle sessions.

## 24. Open Questions

- Should remote blocking reuse `mcp_blocked_projects`, or should remote control have its own block set?
- Should arbitrary command launch ever be available from iOS, or only presets?
- Should iOS be allowed to add a new Mac project by typing a path, or should adding projects require Mac confirmation?
- Should opening a session from iOS resize the hosted viewport, or should iOS render the Mac/session host's current viewport without changing it?
- Should iOS show full terminal scrollback in V1 or only current viewport plus output preview?
- Should remote write actions be confirmed on Mac for first use per device?
- Should pairing require both QR scan and Mac-side approval after the iPhone connects?
- Should folders be a shared on-disk Rust state concept or remain native overlay initially?
- Should provider-specific approval buttons wait for richer hook payloads?

## 25. Immediate Engineering Backlog

1. Keep `UnpeelShared` compiling as a Foundation-only module.
2. Move only audited pure types into `UnpeelShared`.
3. Add Mac-side adapters from `Project`, `Preset`, `SessionEntry`, and viewport snapshots to shared remote DTOs.
4. Define the remote bridge storage location and device credential shape.
5. Add a Mac settings panel section for Remote Control.
6. Add a development-only localhost bridge endpoint for `bootstrap.snapshot`.
7. Add a mock iOS package/app target that depends on `UnpeelShared`.
8. Implement session inbox UI against mock data.
9. Wire pairing once the bridge exists.
10. Harden auth before enabling LAN access.
