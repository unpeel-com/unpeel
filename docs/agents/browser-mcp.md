<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Built-in Browser MCP (Browser Access)

Unpeel ships a second first-party MCP server that gives an agent session a real
browser. Design rationale and verified engine findings:
the private "browser-mcp-deep-check" design record (the engine has **no MCP mode of its
own** — Unpeel authors the server and owns the tool schema).

> **Security scope:** the separate browser profile isolates browsing data from
> the user's normal browser; it does not isolate the hosted process. On/Ask/Off
> and site rules are cooperative controls for agents using Browser MCP, not a
> sandbox against commands running as the same OS user. Same-user code can read
> local Unpeel state and invoke local tools outside this wrapper. Do not call
> these settings a hard security boundary.

- Server: `crates/unpeel-core/src/browser_mcp.rs`, run as
  `unpeel-host __browser_mcp__` (stdio JSON-RPC, hand-rolled like `mcp_host.rs`).
  Caller identity via `UNPEEL_SESSION_ID`. 13 tools (`browser_open`,
  `browser_snapshot`, `browser_click`, `browser_fill`, `browser_type`,
  `browser_press`, `browser_get`, `browser_screenshot`, `browser_wait`,
  `browser_scroll`, `browser_console`, `browser_close`, `browser_context`), each
  translated into one CLI invocation of the bundled `agent-browser` engine. The
  server builds the argv itself, so agents can never pass policy-overriding
  flags. `browser_context` is always callable (it explains access state); the
  rest are gated per call.
- Engine: `agent-browser` in its experimental `--native` mode
  (`AGENT_BROWSER_NATIVE=1`) — a pure-Rust CDP daemon driving the **system
  Chrome/Chromium**, no Node/Playwright/Chromium download. Verified live: open,
  snapshot refs, screenshot, allowed-domains enforcement. Binary resolution
  (`resolve_engine_binary`): `UNPEEL_BROWSER_BIN` env → sibling of `unpeel-host`
  (packaged layout) → `~/.unpeel/browser/bin/agent-browser` → PATH (dev).
  `agent-browser` is pinned to **0.34.0**. Every Unpeel Session still gets an
  independent engine daemon, `unpeel-<session-id>`, with sockets under
  `~/.unpeel/browser/sockets`; by default those daemons use pinned-tab mode to
  attach to one project-owned Chrome process, so each Session controls exactly
  one tab in the project's shared browser window.
- Remote CDP mode: a Host provisioner may write an owner-only regular file at
  `~/.unpeel/browser/remote-cdp.json` (`0600`, never a symlink):

  ```json
  {
    "schema": 1,
    "endpoint": "wss://provider.example/cdp?token=secret",
    "provider": "upstash"
  }
  ```

  When present, the same native `agent-browser` binary receives the endpoint
  through its `AGENT_BROWSER_CDP` launch environment instead of launching
  system Chrome. Authenticated `wss://` endpoints are accepted, as is a bare
  numeric port that `agent-browser` resolves strictly on loopback. The latter
  is for Host platforms that provision Chromium inside the same container;
  Upstash Browser currently uses
  `{"schema":1,"endpoint":"9222","provider":"upstash"}`. Credentialed
  endpoints are read per call, replaced with `<remote-cdp-url>` in engine
  output, never placed in argv, app-state, or a Controller DTO, and represented
  in daemon binding state only by SHA-256. Each Session attaches with pinned-tab
  semantics, so concurrent agents cannot silently take over one another's tab.
  A rotated endpoint replaces a verified browserless per-session daemon before
  the next command; switching away from a live browser binding fails closed
  until `browser_close`. The endpoint remains visible to the same Unix user through
  the environment/config file, so Browser MCP's existing cooperative-security
  caveat still applies. The provider owns browser-process/profile isolation
  and remote downloads; Unpeel still owns MCP access policy and writes screenshots into
  the Session artifact/gallery paths. Provisioning and refresh code must write
  the file atomically with `0600` permissions and must not place the provider's
  broader API key in the Box.
  Live Upstash verification on 2026-08-16 found that its SDK-minted WSS URL
  works with `playwright-core`, while `agent-browser` 0.27.0 and the
  then-pinned 0.31.1 both received HTTP 400 from that proxy. The Box-local `9222` binding
  passed the complete Browser MCP path and is the supported Upstash form until
  that client/proxy mismatch changes.
  `UNPEEL_TEST_REMOTE_CDP_URL='wss://…' scripts/verify-browser.sh`
  exercises this path end to end when a disposable provider endpoint is
  available; the ordinary smoke remains local-only when it is absent.
- Artifacts: with the default Settings ▸ Sessions use auto-gallery toggle,
  screenshots land in
  `~/.unpeel/app-sessions/<id>/artifacts/browser/screenshots/`; when disabled,
  ordinary captures land in the unlisted `.../browser/captures/` directory
  until the agent calls Sessions `add_to_gallery` (or requests the screenshot
  with `gallery: true`). In separate-per-Session mode downloads use
  `.../downloads/` via `AGENT_BROWSER_DOWNLOAD_PATH`; the default shared
  project browser keeps downloads under
  `~/.unpeel/browser/projects/<project-key>/downloads/`. Tools return their
  paths. Phone screenshot requests explicitly set `gallery: true`.
- Grants (`state.rs`, reworked 2026-07-18): `BrowserAccess` is now
  `off`/`ask`/`on` — the same three-mode picker as computer use, with **On
  ("Allow") as the default** (the engine uses an Unpeel-managed project
  profile with no access to the user's own browser, so it does not expose
  personal logins; Settings ▸ Browser ▸ Off is the master disable).
  Under `ask`, a session's first browser action blocks on an approval alert
  (`/mcp/approve-browser`, `MCPBrowserApproval.swift`); Allow is remembered
  in `browser_approvals` with the same prune/carry lifecycle as
  `computer_approvals`, revocable in Settings ▸ Browser. `On` serializes as
  `"on"` for wire compat; `from_state_str` accepts `"allow"` as a synonym.
  Browser MCP is also **experimental** in the native app
  (`ExperimentalFeature.browserMcp`, env `UNPEEL_DEV_BROWSER_MCP=1`), gating
  the Settings ▸ Browser tab and native launch injection. Headless/CLI
  launches have no native UserDefaults feature layer, so they derive launch
  injection directly from the shared `browser_default_access` setting. There
  is still **no per-session override map** (the legacy `browser_access`
  deviations map is decode-tolerated, never written). Elevated future modes
  (shared/copied Chrome profile, live CDP) must stay opt-in. The server
  re-reads access and approvals per call, so changes apply live.
- Injection is per-session at launch (`SessionHostLaunch.browser_mcp_enabled` =
  native experimental flag on, where applicable, &&
  `browser_default_access != off`; malformed explicit access fails closed),
  recorded as the `browser_mcp_enabled` domain grant; the separate
  `browser_client_registered` bit records automatic provider setup. Since 2026-07-18
  the browser tools ride the **unified `unpeel` server** for new launches (the
  `browser` action tool, advertised only when the domain grant is set — see
  the unified-surface note in the Sessions MCP section); there is no separate
  per-provider browser config anymore. The standalone `__browser_mcp__` argv
  and the legacy per-domain config files remain for sessions launched
  pre-unification.
  Flipping the app-wide default **on** takes effect in a newly configured
  terminal (there is no per-session reload banner); **off** applies live
  through the per-call gate.
- Lifecycle: the engine daemon + project Chrome deliberately outlive the
  provider CLI, so `UnpeelStore.killAndCleanup` also spawns
  `unpeel-host __browser_cleanup__ <id>`. Cleanup explicitly closes that
  Session's pinned tab before its daemon, removes every daemon sidecar, and
  drops its project membership. Other Session tabs stay alive; the project
  owner closes Chrome only when the final live/recent Session member is gone.
  A one-hour owner idle timeout is the crash fallback. Browser access is
  app-wide, so a restart relaunches under the same global default — no
  per-session grant to carry.
- Engine options (`AppState.browser_settings`, `BrowserSettings` in
  `state.rs`): `headed` (default true — visible window; false = headless,
  screenshots still work), `allowed_domains` (engine-enforced allowlist with
  wildcards; blocks navigation, sub-resources, and WebSockets), `profile_mode`
  (`"project"`, the default, = one persistent Unpeel-managed browser
  window/profile per project tree under `~/.unpeel/browser/profiles/`, with a
  pinned tab per Session and shared cookies/logins; `"session"` = a separate
  ephemeral browser per Session — neither ever uses the user's own Chrome
  profile), `executable_path` (custom Chromium-based browser; empty =
  auto-detect Chrome), `show_cursor` (default true — before each
  click/fill/type-into-target the server injects a fixed-position pointer
  overlay into the page and glides it to the target's center via `get box
  --json` + `eval`, so a human watching the headed window can follow the
  agent; strictly best-effort and headed-only, `maybe_show_cursor` in
  `browser_mcp.rs`). All read per engine invocation (`load_options` in
  `browser_mcp.rs`) so Settings changes apply to the agent's next browser
  action with no restart. The server also passes the app's `theme`
  (dark/light) as the page color scheme and `AGENT_BROWSER_MAX_OUTPUT`.
- Native UI: Settings ▸ **Browser** only (engine status probe, the single
  app-wide access picker, Options — window/browsing data/clear/browser app,
  Site access rules) in `SettingsView.swift` (`BrowserSettingsPanel`). There is
  no sidebar Browser Access menu. `UnpeelStore.setDefaultBrowserAccess` /
  `updateBrowserSettings` / `clearBrowserProfiles` are the write paths.
- **The Node "full engine" mode is ruled out** (product decision 2026-07-02):
  Unpeel stays a lightweight Swift app and will never ship or require a Node
  runtime. Everything that lives only in the engine's Node/Playwright daemon
  — video recording (native `record` silently writes no file), traces,
  viewport WebSocket streaming (`AGENT_BROWSER_STREAM_PORT` is a no-op in
  native mode), and the iOS Simulator path — is therefore "needs an upstream
  native-daemon contribution" (CDP `Page.startScreencast` is the natural PR),
  never "detect/enable Node". Deferred by choice, not blocked: action
  policies/confirmations, the engine auth vault, per-origin header injection,
  extensions.
- Debugging: `browser-mcp` lines in `~/.unpeel/hooks/trace.log`. Test with
  `printf '...' | UNPEEL_SESSION_ID=<id> unpeel-host __browser_mcp__`.
- **Host-owned install (2026-09-03, open-source prerequisite 2):** the pin
  is `protocol/browser-engine-v1.json` (`version`, `license`, the Apache-2.0
  `notice` URL + sha256, and one `{platform, url, sha256}` per
  `darwin-arm64` / `darwin-x64` / `linux-x64` / `linux-arm64`, pointing at
  the upstream GitHub release assets — byte-identical to the npm package's
  `bin/` binaries; the two darwin hashes are the ones `build-app.sh` checks).
  `unpeel_core::browser_engine` embeds it (`pinned()`) and installs the
  platform binary into `~/.unpeel/browser/bin/agent-browser`
  (`ensure_installed(home)`): accept a copy whose sha256 matches; otherwise
  stream the download over the rustls `http_fetch::get_to_file` path
  (redirects followed, 64 MiB cap, one 64 KiB read buffer straight into a
  `.part` file with an incremental sha256 — the body never exists in memory,
  so a worker that installs at start keeps its footprint; a failed or
  mismatched download removes the `.part`), verify the hash **before** the
  rename into place, write
  `LICENSE-agent-browser.txt` (hash-verified) and `agent-browser.version`
  next to it, `chmod 755`, one `browser-engine` line in
  `~/.unpeel/hooks/trace.log`. An exclusive flock on `browser/bin/.lock`
  serialises installers: a second one waits, re-verifies, and finds the
  first one's work. No Node, no npm, no JS shim, ever.
- **When it runs:** (a) `unpeel browser install [--check] [--json]`
  (`docs/agents/cli.md`); (b) the workspace worker calls `ensure_installed`
  once at start on a background thread and publishes
  `serve.json.browserEngine = {state: ready|installing|failed, version,
  path, error}` (additive; a failure is a trace line + that status, never a
  startup failure — `docs/agents/serve.md`). `UNPEEL_BROWSER_ENGINE_INSTALL=0`
  opts the worker out (state `disabled`, no thread, no network; benchmarks
  and hand-managed Hosts); (c) the MCP server resolves the
  engine per call.
- **Resolution order** (`browser_engine::resolve`, shared by the MCP server,
  the CLI verb, and `verify-browser.sh`): `UNPEEL_AGENT_BROWSER_BIN` (the
  older `UNPEEL_BROWSER_BIN` still works) → the managed
  `~/.unpeel/browser/bin/agent-browser` **only if it verifies** against the
  pin (a stale copy is skipped, not used) → `agent-browser` next to the
  running `unpeel-host` (the app bundle; a compatibility candidate until the
  repo split removes bundling) → `PATH`. A missing engine is the same
  clear MCP error as before plus the `unpeel browser install` hint and, when
  the managed copy exists but is stale, its recorded version.
- **Linux / no browser:** the engine drives a system Chrome/Chromium and
  Unpeel never installs one. `unpeel browser install [--check]` exits 4 and
  the MCP error names what was looked for (`google-chrome`,
  `google-chrome-stable`, `chromium`, `chromium-browser`, `chrome`,
  `brave-browser`, `microsoft-edge` on PATH; the `/Applications` bundles on
  macOS) when none exists. `linux-cli.yml` proves the pinned linux-x64
  binary installs and runs (`agent-browser --version`) in the same Bullseye
  image the archive is built in, and that `--check` reports the missing
  browser there.
- Bundling: none since 0.5.0 — `build-app.sh` no longer copies an engine
  or its notice into the app; the Host-installed copy is the engine, and
  Settings ▸ Browser shows `serve.json.browserEngine` (ready / installing /
  failed / disabled with the error and the `unpeel browser install` fix).
  A copy next to `unpeel-host` is still honoured as a compatibility
  resolution candidate for older bundles.
- **Engine bump procedure:** update `version`, every `sha256`, and the
  `notice` in `protocol/browser-engine-v1.json` from the real release assets
  (`shasum -a 256` of the GitHub assets; they must equal the npm `bin/`
  files), bump `AGENT_BROWSER_EXPECTED_VERSION` + hashes in `build-app.sh`
  while bundling remains, run `cargo test -p unpeel-core browser_engine`,
  then `clients/native/verify-browser.sh` against a blank home.
- Shared-project **logins** persist in the project profile in 0.34.0 (the live
  smoke proves a cookie survives a full browser restart). Unpeel also keeps
  the engine's encrypted state save/restore enabled as a recovery layer
  (`AGENT_BROWSER_SESSION_NAME=unpeel-proj-<root>` plus a per-install
  `AGENT_BROWSER_ENCRYPTION_KEY` →
  `~/.agent-browser/sessions/*.json.enc`). This remains entirely separate
  from the user's own browser profile. Do not re-attempt cookie-DB copying;
  the old native engine's mock-keychain behavior made it unsafe and it is not
  needed for project sharing.
- **Shared project window (implemented 2026-08-17):** `profile_mode=project`
  launches one project-owner engine/Chrome with the persistent profile, reads
  its loopback WebSocket CDP endpoint, and attaches each Session's own daemon
  with `AGENT_BROWSER_PIN_TAB=1`. The binding marker contains only a project
  key plus an endpoint hash; the validated loopback endpoint and owner record
  are owner-only files under `~/.unpeel/browser/projects/<key>/`. Worktree
  projects resolve to their top-level parent, matching the existing project
  tree scoping. Site allowlists currently cannot be combined with an attached
  CDP browser, so setting site rules deliberately falls back to the separate
  per-Session browser mode and reports that limitation in `browser_context`.
