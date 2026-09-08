# Unpeel local patches

Vendored from https://github.com/Lakr233/libghostty-spm at tag `1.2.4`
(`.git` and `Example/` removed). UnpeelNative consumes this copy via a
path dependency because these changes are not upstream yet:

> **libghostty core upgraded to ghostty tip (2026-07-09).** The binary
> XCFramework is no longer Lakr233's `storage.1.2.4` release — it is built
> from ghostty tip (commit `2da015cd6`, which includes the ~1.5–6x VT
> throughput optimizations) via `./build.sh --skip-tests`. The patched binary
> from Unpeel commit `fdf68f0` is hosted at the immutable R2 key
> `stable/vendor/GhosttyKit-2da015cd-unpeel-fdf68f0.xcframework.zip`, and
> `Package.swift` consumes it as a URL/checksum binary target. Build
> prerequisites for regenerating it on this machine:
> - **zig 0.15.2 exactly** (ghostty pins it). Use Homebrew's `zig@0.15`
>   keg (`PATH="/opt/homebrew/opt/zig@0.15/bin:$PATH" ./build.sh …`). The
>   official ziglang.org 0.15.2 tarball cannot link anything on macOS 26
>   (undefined libSystem symbols) — do not use it.
> - **Xcode Metal Toolchain** (Xcode 26 ships it as a separate download):
>   `xcodebuild -downloadComponent MetalToolchain`.
>
> The tip source lives in `References/ghostty-upstream` (gitignored) with
> the full Unpeel patch set applied to its working tree; that state is
> snapshotted in `UNPEEL-tip-rebase.diff` (checked in — re-apply it to a
> fresh tip checkout to reproduce). `Patches/ghostty/0002-host-managed-io`
> and `0003-prebuilt-framedata` moved to `Patches/superseded-1.2.4/`; their
> content is folded into the rebase diff (tip now has
> `src/termio/HostManaged.zig`). The remaining `Patches/ghostty/*.sh`
> scripts are idempotent and still run.
>
> When the core patch set changes, rebuild the XCFramework, upload the zip
> under a new immutable commit-named R2 key, and regenerate `Package.swift`
> with `Script/build-manifest.sh`.

1. **Scrollbar metrics exposed** — `GHOSTTY_ACTION_SCROLLBAR` was falling
   into the `default:` (log-only) case. Added:
   - `TerminalScrollbarMetrics` + `TerminalSurfaceScrollbarDelegate` in
     `Sources/GhosttyTerminal/Surface/TerminalSurfaceViewDelegate.swift`
   - dispatch case in
     `Sources/GhosttyTerminal/InMemory/TerminalCallbackBridge.swift`
   Used by Unpeel's terminal scroll-to-bottom button.

2. **`TerminalSurface.performBindingAction` made `public`** in
   `Sources/GhosttyTerminal/Surface/TerminalSurface.swift` (was internal)
   so the host can run `scroll_to_bottom`.

3. **Synchronous render exposed.** `AppTerminalView.renderImmediately()`
   (new `open func`) calling `TerminalSurfaceCoordinator.renderImmediately()`
    (was `private`, now internal). The default wakeup path coalesces the first
    refresh after a view re-attaches on the main queue, so swapping a retained terminal view into
   the hierarchy presents the stale pre-detach drawable for a frame or
   two. Unpeel calls this right after adopting + focusing a pane on
   session switch so the swap transaction already contains a fresh,
   focused-cursor frame.

If you upgrade the vendored copy, re-apply these (or upstream them first —
both are small and generally useful).

4. **Forced refit exposed** — `AppTerminalView.forceRefit()` (new `public
   func`) calling `TerminalSurfaceCoordinator.forceRefit()` (new). A
   `fitToSize()` after re-attaching a retained view sends ghostty the
   same pixel size it already has — a no-op — so layout drift picked up
   while the view was detached/paused survives until the user manually
   resizes the window. `forceRefit` nudges the surface one pixel
   narrower, then synchronizes back to the true size, so ghostty runs
   its real resize path; both writes land before the next draw. Unpeel
   calls this after every session switch.

5. **Ghostty resource teardown moved off the main actor** —
   `TerminalSurfaceCoordinator` now detaches a raw surface on the main
   actor, then frees it on a serial utility queue. `TerminalController`
   uses the same queue for `ghostty_app_free` / `ghostty_config_free`, so
   app teardown stays ordered after surface teardown. This avoids macOS
   hangs where AppKit autorelease cleanup deallocated `AppTerminalView`
   on the main thread and `ghostty_surface_free` blocked waiting for
   termio backend threads. Removing a controller (`controller = nil`) must
   explicitly select this queued path; live controller/config rebuilds stay
   synchronous because they immediately reuse the coordinator.

6. **Coalesced macOS scrollback repaint.** `AppTerminalView.scrollWheel`
   requests a refresh (`noteRenderActivity()`) after forwarding scroll input.
   Wheel-event bursts share one queued refresh on main. Later PTY output
   drives the core renderer, and host-fed writes explicitly request refreshes.

7. *(cherry-pick, 2026-07-09)* **Upstream "Fix AppKit selection copy leak"
   (Lakr233/libghostty-spm#23, commit `65051461`)** applied — landed upstream
   after our 1.2.4 vendor point, so it is already present in upstream tags
   ≥1.2.6 (drop this note on the next vendor sync). Copy now routes through
   ghostty's `copy_to_clipboard` binding + the wired clipboard callbacks
   instead of a manual `readSelection()` pasteboard write, and
   `supports_selection_clipboard` is enabled.

8. **Synthetic key press exposed** — `TerminalSurface.sendKeyPress(keycode:
   mods:)` + `sendControlKeyPress(keycode:)` (new `public` funcs wrapping the
   internal `sendKeyEvent`) synthesize a press+release through ghostty's key
   encoder, so the bytes honor whatever keyboard protocol the running
   program negotiated (legacy vs kitty). Used by Unpeel's jump-to-bottom
   button to fake ctrl+End — sending raw `ESC [1;5F` via `sendText` was
   mangled into literal text for kitty-protocol TUIs like Claude Code.

9. **Event-driven refresh and hidden-pane occlusion** (2026-09-08).
   `TerminalRenderScheduler` replaces the one-second display-rate pump.
   Render actions, wakeups, scroll input, and completed host writes request
   one refresh on the next main-queue turn. Requests before that delivery
   share the pending refresh, without adding a timer delay. No refresh
   tail or wrapper display-link subscription remains while idle.
   The old claim of off-main delivery was incorrect: pinned MSDisplayLink
   2.1.0 delivered the relay callbacks on main. This scheduler also delivers
   on main; the core still owns asynchronous rendering and vsync.

   Hidden ancestors, detached views, and window occlusion stop rendering,
   cancel queued refreshes, and call `ghostty_surface_set_occlusion(false)`.
   `ghostty_app_tick` stays wakeup-driven for every live app regardless of
   visibility, so replay, title, exit, and other mailbox callbacks keep
   progressing. Host bytes still parse while hidden; the scheduler ignores
   their refresh requests until presentation resumes. No raw surface handle
   crosses threads in this scheduler. Teardown cancels pending work before
   freeing the surface. AppKit hide/unhide, reparent, and window-occlusion
   callbacks update presentation independently of explicit host visibility.

   Synchronous resize and adoption draws remain available. Adoption requests
   a new refresh even without new output, and its synchronous draw cancels
   any redundant queued refresh. Host-write/attach notification ordering,
   scheduler coalescing/cancellation, and hidden callback delivery have unit
   coverage. Live Metal replay must still verify scroll, cache adoption,
   synchronized output, and remote replay freshness; unit tests do not
   establish presentation quality or GPU savings.

10. **Callback userdata lifetime hardened** — app wakeup callbacks now use a
   retained `TerminalControllerCallbackContext` that weakly references the
   Swift controller and survives until `ghostty_app_free` completes. Surface
   callbacks now retain their `TerminalCallbackBridge` until
   `ghostty_surface_free` completes. Late callbacks during async teardown
   become no-ops instead of rehydrating freed Swift objects.

11. **UIKit committed text is not paste** — software-keyboard and committed
    IME text now bypasses `ghostty_surface_text` for host-managed terminals and
    writes UTF-8 bytes directly to `InMemoryTerminalSession`.
    `ghostty_surface_text` intentionally applies clipboard semantics and wraps
    every call in bracketed-paste markers when mode 2004 is active; OpenCode
    interprets a bracketed whitespace-only input as a request to paste the
    clipboard, so pressing Space pasted the clipboard contents. The explicit
    Paste accessory still uses `surface.sendText` and retains bracketed-paste
    behavior.

12. **Resize presentation: no stretch, same-turn draw** (2026-08-05, macOS) —
    live window resizes showed the previous drawable STRETCHED to the new
    bounds for a frame before the fresh draw landed ("shaking" text).
    Three changes in `Platform/AppKit`:
    - `AppTerminalView.commonInit` sets `metal.contentsGravity = .topLeft`
      so a stale drawable is never scaled to the new bounds.
    - `setFrameSize`/`layout`/`viewDidChangeBackingProperties` call
      `core.renderImmediately()` instead of `core.requestImmediateTick()`,
      drawing in the same runloop turn as the frame change instead of one
      main-queue hop later.
    - `updateMetalLayerMetrics` guards `contentsScale`/`drawableSize`
      writes behind equality checks — `drawableSize` writes invalidate the
      drawable pool even for identical values, and the method wrote the
      same CAMetalLayer twice per resize frame (via `layer` and the cached
      `metalLayer` ivar).

13. **Point→cell mapping honors an explicit grid origin** (2026-08-05,
    macOS) — `AppTerminalView.gridOrigin` (nil = legacy centered-grid
    assumption). Unpeel switched its panes to
    `window-padding-balance = false` (balanced padding re-centers the grid
    every resize frame — the "text shaking" during window drags), so the
    grid's top-left corner is now the fixed padding, which the host passes
    in for `gridCell(atViewX:viewY:)` (cmd-click file detection).

14. **Search actions exposed** (2026-08-12) — the four search action tags
    were falling into the `default:` (log-only) case. Added
    `TerminalSurfaceSearchDelegate` in
    `Sources/GhosttyTerminal/Surface/TerminalSurfaceViewDelegate.swift`
    and dispatch cases for `GHOSTTY_ACTION_START_SEARCH` / `END_SEARCH` /
    `SEARCH_TOTAL` / `SEARCH_SELECTED` in
    `Sources/GhosttyTerminal/InMemory/TerminalCallbackBridge.swift`.
    libghostty runs the search itself (query via the `search:<text>` /
    `navigate_search:*` / `start_search` / `end_search` binding actions
    through the already-public `performBindingAction`); these callbacks
    give the host the "3 of 17" counters. Match totals are collected
    during drawing, so they only fire while the surface renders. Used by
    Unpeel's ⌘F find bar.

15. **Drift-aware refit** (2026-08-20) —
    `TerminalSurfaceCoordinator.refitIfDrifted()` +
    `AppTerminalView.refitIfDrifted()` (public). Unpeel used to run patch 4's
    `forceRefit` after every session switch, which costs two full ghostty
    resize passes plus PTY winsize churn (double SIGWINCH → the running TUI
    repaints twice) even when nothing changed — the main reason switching
    sessions felt slower than Ghostty's instant tabs. The new entry point
    compares the view's current pixel size and scale against what the
    surface last received (`lastSentPixelSize`/`lastSentScale`) and only
    falls through to `forceRefit` when they differ (layout/display change
    while the view was detached or paused); the clean re-adopt skips the
    nudge and just schedules a tick.

16. **Display link follows visibility, not focus** (2026-08-20, core patch in
    `UNPEEL-tip-rebase.diff`, marker `LIBGHOSTTY_SPM_DISPLAY_LINK_VISIBLE_PATCH`,
    `src/renderer/generic.zig`) — upstream runs each surface's vsync
    CVDisplayLink only while the surface is focused AND visible, stopping it
    on focus loss. Ghostty's app shows one focused surface, so that's fine
    there; Unpeel displays several surfaces at once, and an
    unfocused-but-visible surface fell back to coalesced change-driven
    draws — sustained streams (kitty image apps) presented at a fraction of
    display rate. `setVisible` now starts/stops the link on visibility
    alone and `setFocus` leaves it untouched, so every visible surface
    draws at display cadence exactly like the Ghostty app's focused one;
    occluded surfaces still stop their links.

17. **Present drift: rescale instead of discard** (2026-08-20, core patch in
    `UNPEEL-tip-rebase.diff`, marker `LIBGHOSTTY_SPM_PRESENT_DRIFT_PATCH`,
    `src/renderer/metal/IOSurfaceLayer.zig`) — upstream macOS discards any
    asynchronously drawn frame whose IOSurface pixel size differs at all
    from layer bounds×contentsScale (guard against mid-resize jank). An
    embedder whose surface size disagrees with the layer by a rounding
    pixel then renders at full rate while every async present is silently
    dropped — only synchronous draws reach the screen, which reads as
    inexplicably low terminal FPS. Small drift (≤8px) now applies the same
    contentsScale-recalculation recovery iOS already used, with a
    rate-limited stderr log (`[ghostty-embed] IOSurfaceLayer size drift`)
    so the condition is observable; large drift (a real resize race) still
    discards.

18. **Renderer display link bound to the window's display** (2026-08-20) —
    the wrapper never called `ghostty_surface_set_display_id`, so each
    surface's vsync CVDisplayLink ran against a default display instead of
    the one hosting the window (upstream Ghostty's SurfaceView calls it on
    every screen change precisely so the link uses the right refresh rate —
    SurfaceView_AppKit.swift:784-798). On ProMotion or multi-display setups
    that caps vsync-driven draws at the wrong rate: stable, inexplicably
    lower FPS with an otherwise healthy renderer. Added
    `TerminalSurface.setDisplayID`, coordinator hook `currentDisplayID` +
    `refreshDisplayBinding()` (bound at surface rebuild), and AppKit wiring
    from `viewDidMoveToWindow` + `windowDidChangeScreen` (logs the screen's
    `maximumFramesPerSecond` alongside).

19. **Kitty image loading limits survive RIS** (2026-08-20, core patch in
    `UNPEEL-tip-rebase.diff`, marker `LIBGHOSTTY_SPM_KITTY_LIMITS_RESET_PATCH`,
    `src/terminal/Screen.zig` — upstreamable bug fix) — `Screen.reset()`
    reinitializes kitty image storage as `.{ .dirty = true }`, silently
    resetting `image_limits` from the `.all` Termio configured back to the
    default `.direct` (no file / temp-file / shared-memory transmission).
    `ImageStorage.setLimit` preserves limits across a wipe; `reset` forgot
    to. Ghostty's own app never receives RIS so upstream never noticed —
    but Unpeel's attach client emits RIS on every reattach, so every
    surface permanently lost file/shm capability after first attach:
    kitty graphics apps (terminal-browser) probed `t=f`/`t=s`, got
    `EINVAL: unsupported medium`, and fell back to inline base64 pixels
    through the PTY (~17fps self-paced). Diagnose with a raw
    `ESC_Gi=1,s=1,v=1,a=q,t=f,f=24;<b64 path>ESC\` probe: `OK` = fast
    path available, `EINVAL` = limits lost.

20. **Read-only viewport mouse hit exposed** (2026-08-27, macOS) —
    `AppTerminalView.viewportRowHit(at:)` maps an `NSEvent` to the visible
    terminal row, column, and row text using the same explicit grid origin as
    cmd-click detection. Unpeel uses it to resolve an App-published,
    session-local path drag map at mouse-down; ordinary terminal mouse input
    remains unchanged when no mapped row is present.

21. **Command-click text offsets** (2026-09-07, macOS) — file detection
    receives a Swift Character offset from Ghostty's selected row prefix,
    rather than treating grid columns as string offsets after CJK/emoji.
    `readViewportRow` accepts an optional final column for this translation.
    Pointer hit testing also
    rejects points in the top/left padding before integer conversion.
    `viewportRowHit` continues to report grid columns for semantic drag maps.
