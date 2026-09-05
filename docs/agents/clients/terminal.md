<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Terminal Stack

The native terminal is a **libghostty** surface (GhosttyKit), Metal-rendered, not xterm.js.

- The Swift app embeds a Ghostty terminal surface per visible session.
- The surface runs `unpeel-attach <session-id>` (see `crates/unpeel-attach`), which:
  - **snapshot attach (2026-09-02, Round 2):** after the startup resize it
    sends the additive `{"type":"snapshot"}` control command; the Host renders
    its resident libghostty-vt state (cells, styles, wide chars, scrollback,
    every non-default mode, scroll region, tabstops, charsets, pen, cursor)
    with the library's own VT formatter (`terminal_viewport::snapshot_vt`)
    and answers one JSON header line `{journal_offset, cols, rows,
    bytes_len}` followed by the raw bytes — offset capture and render happen
    under one viewport lock, so the offset is exactly where the snapshot's
    knowledge ends. The client resets its terminal, applies the snapshot, and
    subscribes the output stream from that offset. Attach is therefore exact
    for incremental-repaint TUIs regardless of how much journal tail is
    retained (verify-attach.sh proves screen equality against the Host's own
    viewport mid-repaint; locally ≈0.5 ms request → last byte flushed). Not
    carried by a snapshot: the palette and OSC 7 (the client keeps its own
    theme/cwd), DECSCUSR cursor shape, kitty graphics, and the pending-wrap
    flag (see `SnapshotVt` docs).
  - **fallback:** a Host that predates the command closes the connection
    without a reply (serde rejects the variant), the manifest is `exited`, or
    `UNPEEL_ATTACH_SNAPSHOT=0` is set → today's path unchanged: replay a
    boundary-aligned tail of `output.bin` (`--replay-bytes`) after the
    manifest's `terminal_modes` preamble. `UNPEEL_ATTACH_TIMING_FILE` appends
    one `attach_us=… path=snapshot|tail` line per attach for measurement.
  - then bridges stdio ↔ `session.sock` for live I/O and resize.
  - The remote `__remote__` WS streamer still replays a journal tail on
    subscribe: its binary frames derive the client cursor from
    `start_offset + payload length`, so a non-journal snapshot payload needs
    an additive baseline frame type first (not done in Round 2).
- The hosted PTY writes a **logically append-only, physically bounded** terminal
  journal to `output.bin`. Lifetime byte offsets and the file's logical length
  stay monotonic so attach, mobile, SSH, Relay, and WebSocket cursors remain
  stable, while the Host hole-punches old blocks and keeps roughly a 64–72 MiB
  readable suffix per Session. `output-retention.json` records the earliest
  retained logical offset; a reader whose cursor aged out rebases to an aligned
  tail and resets its VT before feeding it. Replay floors are UTF-8/VT-safe in
  ordinary output, with a bounded reset checkpoint for malformed unterminated
  control strings so hostile repaint loops cannot defeat the disk cap.
- `output.bin` is recovery scrollback, not the durable semantic transcript.
  Exited legacy journals are compacted opportunistically on desktop/headless
  startup. A live Host older than protocol v4 is never rewritten underneath
  its open file descriptor; the normal **Reload Terminal** recommendation
  upgrades it to bounded journaling.
- The native app keeps a small LRU cache of live surfaces and pre-warms on hover (see the surface cache in `apps/native`); evicted-then-remounted surfaces rebuild from the replay tail plus new live output.
- Agent TUIs that repaint the screen in place can still appear to "crop" or "overwrite" detail while streaming — normal terminal behavior; intermediate full-screen redraw states are not guaranteed to survive as scrollback.

### TUI kitty graphics passthrough (removed 2026-09-03)

Before the interactive terminal UI's removal, it composited sessions through
its own in-memory ghostty-vt, which rendered text — not images. Since
2026-08-22 the live-streamed pane (the selected, running, TUI-owned-grid
session — `crates/unpeel-cli/src/stream.rs`, deleted with the TUI) forwarded
kitty graphics instead of dropping them, so a graphics app hosted in a
session (the Surface runtime, icat-style tools) displayed inside the TUI
when the outer terminal spoke the kitty protocol (Ghostty, kitty, herdr):

- `TerminalViewportState`'s kitty APC filter (`terminal_viewport.rs`) still
  strips `ESC _ G … ESC \` from the VT feed, but the opt-in capture
  (`set_graphics_capture`) it used to enable for the TUI's live stream has no
  remaining consumer now that the TUI is gone.
- `crates/unpeel-cli/src/graphics.rs` (`GraphicsForwarder`), which drained
  captures each frame and emitted between the ratatui draw and
  `EndSynchronizedUpdate`, was deleted with the TUI.
- This entire passthrough mechanism (placement anchoring, delete rewriting,
  scrollback/modal hide, query dropping, per-image-id limits) no longer
  exists. There is currently no equivalent kitty-graphics passthrough for any
  frontend. PTY coverage for it (case `graphics`) was removed along with the
  TUI test harness.

### iOS remote terminal direction

The iOS app is terminal-first for the current product pass. Keep the phone
session detail screen focused on the live terminal surface, direct typing,
touch scroll/pinch behavior, session sidebar, and provider-agnostic control.
Do not replace the phone detail view with a semantic chat UI yet.

Semantic transcript reads are still useful, but as a shared supporting API:
session previews, future chat experiments behind a feature flag, debug views,
search/indexing, and MCP `read_transcript`. The implementation lives in
`crates/unpeel-core/src/transcripts/`.

#### Keyboard focus shortens the terminal from the bottom

All in `RemoteGhosttyTerminalView.swift`. Focusing the phone terminal uses the
software keyboard's full on-screen height as the terminal's new bottom edge.
The canvas is top-anchored, so the title and first terminal row stay fixed
instead of the whole terminal sliding upward. In fit-to-screen mode this is an
intentional row-only PTY resize: the TUI gets a genuinely shorter window and
restores its rows when the keyboard closes.

- **Columns stay frozen.** `restingSizingSize.width` is captured while the
  keyboard is down and reused for its whole focus lifetime. Sub-point width
  wobble from the keyboard animation or Ghostty's first-responder metric
  re-report must never change columns or rewrap the transcript.
- **Rows follow the keyboard, at the WILL frame.** The sizing viewport height
  uses the terminal-owned keyboard reserve (the keyboard height minus the
  canvas's own bottom padding, so only the row-quantization remainder shows
  as a gap above the keyboard). The inset keys off the WILL-show/hide END
  frame (`KeyboardHeightObserver.projectedHeight`): the complete final
  layout — viewport, canvas, pan, and the row-only PTY request — snaps in
  one MainActor turn the moment the keyboard announces itself, the keyboard
  animates over the already-settled terminal, and the TUI's repaint races
  the slide. Waiting for the committed did-frame (the old model) reflowed
  the terminal ~300ms after the tap and read as a laggy multi-step resize.
  `RemoteGhosttyRenderer.keyboardActive` clamps a fit request to the current
  remote columns while allowing its derived row count; the resting sizing
  capture is gated on BOTH heights being zero so the WILL-turn's shortened
  viewport is never captured as the resting size. Stale pre-resize Ghostty
  metrics are ignored until the exact requested grid arrives and a timeout
  never applies a fallback canvas. On the Mac, a mounted pane is the sole
  resize owner (the endpoint does not also raw-resize it), and
  `unpeel-attach` coalesces AppKit's settling SIGWINCH burst for 60 ms
  before forwarding the final grid. Each show/hide transition therefore
  reaches the workload as one PTY resize and one TUI redraw.
- **Other keyboards do nothing.** `keyboardOwnsInset` latches only when the
  terminal summoned the keyboard, so a sheet or photo-picker search field
  cannot leave a dead gap in the terminal.

Normal (non-fit) mode remains a pure viewer and never resizes the desktop; it
top-anchors and clips the remote canvas at the keyboard edge. Remote row
resizes remain exclusive to fit-to-screen mode, which carries the desktop
revert banner.

#### Predictive local echo (relay only)

`RemoteTerminalPrediction.swift` + wiring in `RemoteGhosttyTerminalView.swift`.
Over the relay a keystroke's echo pays two WAN traversals, so typed printables
are rendered immediately as an underlined provisional overlay at the surface's
IME caret cell, then reconciled against the parsed viewport when server bytes
land. Safety is a mosh-style confidence gate: predictions stay invisible until
one is CONFIRMED by the real grid, and a contradiction or 2s expiry closes the
gate again — password prompts, vim normal mode, and TUIs that park the caret
elsewhere never show one. The overlay is pure view state (a separate
`ObservableObject` so keystrokes don't invalidate the terminal tree) and never
touches VT state; the tap observes the same memory-session write path that
feeds the ordered input queue. Direct/LAN transports skip it entirely.

Fit ownership follows who is looking: `/mobile/metrics` carries a
`desktopViewing` flag (Mac's `observedSessionID == session` — selected + app
frontmost), and the phone's `autoRefitIfUnwatched` re-asserts the letterbox
whenever the session is unfitted or deviates while the Mac is NOT viewing it
(throttled, ≥3s). The manual fit button therefore only appears while the Mac
is actively viewing the session (its banner-X revert wins) or against an
older Mac that doesn't report the flag.

#### Menu control bar (agent-rendered select menus)

Agent-drawn "pick an option" menus (Codex/Claude numbered prompts) fire **no
hook** — no `Stop`, no `PermissionRequest` — so the activity engine keeps
showing "busy" and nothing flags "waiting for a choice". Do **not** rely on the
`.blocked`/attention state to detect them; it only covers real tool-permission
prompts. Instead the phone detects them from the **rendered viewport text**
(the same `terminal.surface?.readViewportText()` scan the "Jump to bottom" hint
uses, debounced post-feed): a menu advertises itself with a navigation hint
(`↑/↓ to navigate`) plus a select/cancel hint (`Enter to select`, `Esc to
cancel`) on the same or adjacent rows, and must show at least two numbered
choices. Footer-like prose without choices remains writable input. Rows whose Enter action is "view" are
passive status footers, not menus — Claude Code's subagent list pins
"`↑/↓ to select · Enter to view`" for the whole run and must not trip
detection (it falsely flagged attention on every subagent-running session).
`RemoteGhosttyRenderer.menuPromptActive` drives `TerminalMenuControlBar`,
a bottom overlay (shown only while a menu is up and the keyboard is down) with
↑/↓ · Enter · Esc · direct number keys, so a choice is answerable without the
keyboard. Keys go straight to the remote PTY via the ordered write queue;
arrows honor DECCKM (mode 1, tracked in `RemoteTerminalMouseModeTracker`) so
they encode as `ESC O A/B` vs `ESC [ A/B` to match the TUI.
The ordinary software-keyboard accessory contains no arrow cluster; when a
menu becomes active it dismisses writable focus and exposes this menu-only
control bar instead.

#### Session gallery (per-session images)

The phone's gallery (`BrowserGalleryPanel`, opened from the terminal's photo
button) is a **unified per-session image view**, not just the agent's browser
captures. It lists four artifact kinds under `~/.unpeel/app-sessions/<id>/
artifacts/`, newest-first: `browser/screenshots` and `browser/downloads`
(browser-MCP output), `computer/screenshots`, and `uploads` (images the user,
phone, or Sessions `add_to_gallery` action added). Settings ▸ Sessions use can
keep ordinary Browser MCP screenshots out of the gallery; those captures land
under unlisted `browser/captures` until explicitly published. The kind→dir mapping lives in the shared `SessionArtifactStore`
(`SessionArtifacts.swift`), read by both galleries; `/mobile/artifacts` lists
them and `/mobile/artifact` serves bytes. The **desktop app has the same
gallery** (`SessionGalleryPanel.swift`): a photo button at the trailing edge
of the terminal title bar (next to the workspace-open menu in
`TerminalArea.swift`) opens a popover reading the artifact dirs straight off
disk. On desktop the whole chip is **optional and off by default**
(Appearance ▸ "Session gallery", `UnpeelStore.showSessionGallery`) — some
users have their own screenshot tooling; disabling it also disables Session ▸
Take Screenshot… (⇧⌘S greys out via `AppDelegate.validateMenuItem`, since the
chip owns the capture flow). The phone gallery and artifact dirs are
unaffected — the gallery is **always on for mobile**, never gate it there.
The popover shows a grid → enlarged detail, with Add to prompt (types the quoted path into
the session's terminal via `GhosttyTerminalPane.insertAttachablePath`, the
same quoting as a Finder drop), Reveal in Finder, and Delete (which also
reaps legacy on-disk thumbnail variants from older builds). The desktop detail view has
the same **arrow + crop markup** as the phone (`SessionGalleryMarkup.swift`,
the twin of the iOS `ArrowMarkup`/`ArrowGeometry` — keep palette, stroke
formulas, and geometry in step); with edits pending, Add to prompt exports a
full-resolution annotated PNG into `artifacts/uploads/` (the same kind
phone-annotated images land in) and attaches that copy, never mutating the
original. Both gallery buttons **pulse**
(spring scale, ~2s) when a new agent capture lands — kinds in
`SessionArtifactStore.captureKinds` (browser + computer screenshots; never
uploads/downloads). Desktop polls the artifact dirs directly
(`SessionGalleryButton.watchForNewCaptures`); the phone polls
`/mobile/artifacts` (`watchForNewScreenshots` in
`RemoteGhosttyTerminalView.swift`). Keep the two watchers' kind lists and
pulse feel in step.
`/mobile/artifact?max_dim=N` serves a downscaled JPEG variant instead — the
grid-tile path (a full-page PNG screenshot is multi-megabyte and many relay
round-trips; the thumbnail is one). Thumbnails are generated on demand via
ImageIO, cached under `artifacts/thumbs/` keyed by mtime+dimension (stale
variants reaped on regeneration and artifact delete), and never touch the
original file; files at or under one chunk (200KB) skip thumbnailing. Tapping
a tile still fetches the original full-resolution bytes, so markup/crop/"Add
to message" stay lossless.

Phone image uploads (`/mobile/upload?session_id=…` → `saveUploadedImage`) now
land in that session's `artifacts/uploads/` (falling back to the shared
`dropped-images` dir only when no session is supplied), so an uploaded/edited
image is attributed to the session and shows in its gallery — the pasted
composer path just points at the per-session file instead of the global drop
dir. `RemoteMacClient.uploadImage` takes the `sessionID`. The gallery's first
tile is a `PhotosPicker` "+" that uploads into the session. Desktop
drag-and-drop is still global/unattributed and does **not** appear in the
gallery (a separate change). Full-size images support pinch-zoom
(`ZoomableImageView`) and two markup tools above "Add to message" —
`ImageCropView` (adjustable rect → native-pixel crop) and `ImageArrowMarkupView`
(drag-to-draw arrows, flattened at native resolution).
