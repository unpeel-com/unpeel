# App Kit Kitchen Sink

This macOS 14+ SwiftUI executable is a self-contained mini-host for App Kit.
It does not install, import, or launch Unpeel. Run it from the repository root:

```sh
swift/Examples/KitchenSink/run-app.sh
```

The launcher builds the SwiftPM executable, wraps it in a generated ad-hoc
signed `.app`, and opens a new foreground instance with a stable bundle
identity. The generated app stays under `.build` and is replaceable. You can
still use `swift run --package-path swift/Examples/KitchenSink`; the executable
also promotes itself from macOS's default `BackgroundOnly` classification.
Either path lets libghostty, native SwiftUI, and embedded web views own keyboard
focus instead of the terminal that launched the rig.

The first launch fetches `libghostty-spm` 1.5.0 and builds the sibling Usage,
Diffs, GitHub Issues, Markdown, and File Tree Apps plus this repository's
Charts, Todo, Markdown, Media, Surface Planets, and Canvas + Controls examples into
`target/kitchen-sink`. The five sibling repositories must sit beside
`unpeel-app-kit`; Unpeel itself is not installed or launched. Surface examples
are added when their sibling guest artifact exists. Their native and web
presenters are enabled when the sibling
`UnpeelSurfaceKit` XCFramework and `web/pkg` artifacts also exist. Each example
then runs as the direct child of a real Ghostty exec PTY in a private,
short-lived `/tmp/upkit-…` session directory. GhosttyTerminal owns the grid,
selection, clipboard, keyboard/IME input, scrollback, Metal presentation, and
child-process lifetime. An occluded parking window keeps a hidden surface
attached and draining its PTY without spending work on frame presentation.

For each session the mini-host:

- creates `ui.sock` and a random per-session signing key;
- injects `UNPEEL_UI_SOCKET` and `UNPEEL_UI_TOKEN` only into the App process,
  along with its session id/directory;
- retains the signing key and mints short-lived, route-bound participant
  tokens with `UIParticipantTokenIssuer`;
- attaches `UIUnixSessionClient` to the same process shown in the terminal;
- switches among Terminal, Native, Web, and three-way Split lifecycle states
  so the Rust App's `UiBridge::should_render_terminal()` path is exercised;
- renders the same projection with SwiftUI and the repository's real DOM
  components in a credential-free `WKWebView` boundary;
- shows a live, expandable component tree with ids, named slots, actions,
  values, delivery type, and revision beside every presentation; and
- keeps the session directory when the child is killed, so Restart proves
  state restore while producing a new `appInstanceId`.

The bottom harness can disconnect/reconnect the native renderer, restart the
authoritative App process, attach a hidden agent participant with selectable
grants, invoke a semantic action as that agent, inspect presence and final
acks, and distinguish raw snapshots from server deltas. The participant's
personalized title/alt text also proves `publish_to` isolation.

Each of the five sibling sessions also has **Walk every screen**. It drives
deterministic fixture data through the live reducer and records an explicit
terminal-only inventory. Usage covers catalog/detail/Alerts; Diffs and GitHub
Issues cover list and complete Content details; Markdown covers first-run
chooser, Tree picker, new-note Page, editor, context menu, and slash menu; File
Tree covers root and nested Trees. Run the reproducible unattended pass with:

```sh
UNPEEL_KITCHEN_AUTO_WALK=1 \
UNPEEL_KITCHEN_AUTO_EXIT=1 \
UNPEEL_KITCHEN_AUDIT_REPORT=/tmp/app-kit-audit.md \
swift run --package-path swift/Examples/KitchenSink
```

Automatic mode launches only those five audit sessions, prints the report,
and exits. The committed expected result is **Terminal-only surfaces: none**
for every App.

For the Todo session, terminal row clicks and input selection use the public
`Page.layout` / `InputField` hit-test geometry, while native and web controls
emit the same typed actions. For Markdown, click a presentation before typing.
The Ratatui editor owns drag, double-click word, triple-click line, and
multi-line semantic selection; the native pane uses `NSTextView`, and the web
pane uses the App Kit `<textarea>` renderer. Type `/` on an empty line for the
shared block menu and use Backspace at a Markdown marker to turn the block
back into plain text. Text, selection, presentation, and dirty-state changes
all enter the same revision stream. Native and web typing is optimistic and
coalesced before it enters that stream, keeping rapid input responsive without
generating stale same-revision events.

The Charts session cycles Sparkline, BarChart, LineChart, and Gauge with
Left/Right or Enter/Space. The same data-first Page is rendered by Ratatui,
Swift Charts / SwiftUI Gauge, and dependency-free SVG; the harness agent button
invokes the chart's optional semantic activation action.

Surface Planets and Canvas + Controls test the complete three-presenter
composition without Unpeel. The latter overlays the same closed Button slot in
Ratatui, SwiftUI, and DOM while the planet scene stays on Surface/USRF.
Build the sibling guest, Apple library, and browser presenter before launching
Kitchen Sink:

```sh
cargo build --release --manifest-path ../unpeel-surface/Cargo.toml \
  -p surface-planets-example --target wasm32-unknown-unknown
../unpeel-surface/scripts/build-xcframework.sh
(cd ../unpeel-surface && wasm-pack build web --target web --release)
```

The mini-host creates `surface.sock` beside `ui.sock`, injects the fixed logical
viewport and `SURFACE_TERMINAL_PROJECTION=retained-only` into the app process,
retains immutable resources plus the latest scene, and fans the original USRF
packets to two local CAMetalLayer presenters (terminal composition and native
component) and the WebGPU presenter. Pointer and focused key input returns over
USRF. Ghostty remains responsible for the PTY and overlays default-background
terminal cells on the Host's local Surface layer. The producer does not create
a second wgpu/Kitty projection. No presenter receives rasterized frames and the
mini-host never reads back a GPU buffer. Restarting the producer resets native
decoder generations while keeping the broker route alive.

If those optional sibling artifacts are absent, Kitchen Sink does not
advertise the `surface` capability and the complete Ratatui/terminal view is
used. This is capability fallback, never frame emulation.

Set `UNPEEL_KITCHEN_SINK_SESSION=surface` when invoking `run-app.sh` to select
the Surface session immediately for smoke testing; the launcher explicitly
forwards that one test-harness variable through Launch Services.
Use `UNPEEL_KITCHEN_SINK_SESSION=canvas` to open the semantic-controls overlay
session instead.
Use `UNPEEL_KITCHEN_SINK_SESSION=charts` to start on the four-chart showcase.

`TerminalEngineController` is the only libghostty-specific boundary. It uses
the same GhosttyTerminal architecture as the product Host while keeping this
mini-host standalone. `WebComponentPane` is likewise a small WKWebView bridge:
participant credentials never enter JavaScript; snapshots go in and typed
`UIAction` values come out.

The web bundle is checked in so SwiftPM does not require Bun at runtime. After
editing `web/src`, regenerate and type-check it with:

```sh
cd web
bun run build:kitchen-sink
bun run check
bun test
bun run test:browser
```

The browser test drives the shared slash-command conformance fixture through
the real DOM renderer. Install its Chromium runtime once with
`bunx playwright install chromium` if it is not already available.

This package depends on the sibling `UnpeelAppKitUI` library, but is a separate
SwiftPM package and is never a dependency of that library. Its workflow is
manual-only, so ordinary Rust and renderer CI do not fetch or compile
libghostty or WebKit.
