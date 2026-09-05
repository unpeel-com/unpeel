<!-- Split out of the repo-root AGENTS.md (2026-08-31). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Scriptable CLI

`crates/unpeel-cli/src/cli.rs` is the one-shot command dispatcher for the
`unpeel` binary. It is a frontend over shared Host/session contracts, not a
second state implementation. `claim_workspace_flag` runs before dispatch, so
every one-shot verb automatically targets the selected `UNPEEL_HOME` when
prefixed with `unpeel --workspace NAME`.

One-shot writes must use the sanctioned shared-state primitives:

- `unpeel_core::app_state::edit` for `app-state.json`: exclusive flock, raw
  `serde_json::Value` read-modify-write, unknown-key preservation, atomic
  rename, and `state_bus::announce(Change::AppState)`;
- the matching `session_ops` helper for Session markers/order/lifecycle;
- `state_bus::flush()` before process exit. The main dispatcher already does
  this after every handled one-shot command; never bypass that exit path for a
  new mutating verb.

Never deserialize `app-state.json` into `AppState` and serialize it back from a
CLI mutation. That typed round trip would drop fields owned by a newer app.
Never recover from a present-but-corrupt file by seeding defaults. Missing is a
fresh workspace; unreadable or malformed is an error.

### Workspace settings

`crates/unpeel-cli/src/settings_cli.rs` owns this grammar:

```text
unpeel settings list [--json]
unpeel settings get <key> [--json]
unpeel settings set <key> <value> [--json]
```

It deliberately exposes an allowlist, not arbitrary JSON paths:

| CLI key | Stored JSON | Set values | Effective fallback |
| --- | --- | --- | --- |
| `experimental_features.sessions_mcp` | nested boolean | `true`, `false` | `true` |
| `experimental_features.browser_mcp` | nested boolean | `true`, `false` | `true` |
| `experimental_features.computer_use` | nested boolean | `true`, `false` | `false` |
| `browser_default_access` | string | `on`, `ask`, `off` | absent `on`; malformed `off` |
| `mcp_nonchild_write_access` | string | `ask`, `allow`, `deny` | `ask` |
| `theme` | string | `system`, `light`, `dark` | `system` |

Browser access reads use `BrowserAccess::from_state_str` semantics and fail
closed on malformed/non-string state. A nested experimental edit creates the
object only when absent; it refuses to replace a present non-object and keeps
unknown sibling gates. Values are validated before acquiring the write path,
so an unknown key/value leaves the file byte-for-byte untouched.

Human `get` prints the normalized value. JSON `list` is a key/value object;
JSON `get` and `set` return `{"key": ..., "value": ...}`. These shapes and
the parsed `SettingsCommand` separation are the future seam for dispatching
the same grammar through `settings.workspace.set`; local disk semantics are
the current implementation. Do not invent a CLI-only remote settings
protocol.

`experimental_features.sessions_mcp`, `.browser_mcp`, and `.computer_use` are
launch gates (computer use additionally needs `computer_access` ≠ `off` and a
ready adapter on the Host — `computer_mcp::enabled_for_launch_from_app_state`).
Changing them affects Sessions started or restarted afterward, not the
capability set captured by a running Session. Keep that warning in `unpeel
settings --help` and the website CLI page.

### Preset organization

`unpeel presets` mutates the same raw `presets` array through
`app_state::edit`:

```text
unpeel presets star|unstar <label|id>
unpeel presets enable|disable <label|id>
unpeel presets reorder <label|id> <position>
```

Selectors resolve an exact id first, then one exact label; duplicate labels
are rejected instead of changing an arbitrary row. Reorder positions are
1-based at the shell boundary and become a zero-based final array slot
internally, matching the Host protocol's `sortOrder` semantics. Array order is
the display order and the first preset for a CLI is that CLI's default. Star
is the stored `quick_launch` flag; enable is the compatibility `enabled` flag.
Every mutation is idempotent and preserves unknown fields on the row, sibling
rows, and the document.

The current native preset product remains present-or-deleted and normalizes
stored global rows to enabled; headless/Host consumers still honor the
compatibility flag. Do not silently remove the flag or reinterpret disable as
delete. Native convergence, if chosen, is a separate product change.

### Unpeel Link enrollment

`crates/unpeel-cli/src/link_cli.rs` owns `unpeel link` — the scripted
headless spelling of the activation the app's Settings ▸ Remote offers
(originally also offered by the now-removed interactive TUI's Settings ▸
Remote):

```text
unpeel link enroll <key> [--json]
unpeel link status [--json]
unpeel link deactivate
```

Hard rules:

- **One activation implementation.** The CLI composes the exact
  `unpeel_core::license` request/commit primitives the app's Settings ▸
  Remote uses (`request_activation` → `commit_activation` →
  `request_relay_entitlement_for_key` →
  `commit_relay_entitlement_for_activation`), so the locked durable
  suppression record, activation-pending intermediate state, and
  authoritative-rejection semantics are shared byte-for-byte. Never fork a
  second activation path here.
- **Never a client-side gate on anything local.** Enrollment only adds Link
  (off-LAN relay) authority; local/LAN behavior must not consult it.
- Exit codes are script vocabulary: `0` success (for `status`: usable Link
  authority), `1` definitive failure (invalid key, rejection, suppressed),
  `2` transient/retryable. An `enroll` that exits `2` after "activation
  committed" left the durable `activation_pending` state; a retry or a
  running `unpeel serve` finishes it.
- A running serve needs no restart: its driver's Link maintenance observes
  the fresh key/entitlement on its next tick.
- `status` is read-only — it must not mint a Host identity or touch any Link
  file. `enroll` binds to `relay_uplink::ensure_host_id()` like every other
  Link consumer.

Evidence: `tests/cases/link_enroll.py` (shared fixtures in
`tests/link_fixtures.py`, also used by `link_lifecycle.py`).

### Host service packaging (`unpeel serve install`)

`crates/unpeel-serve/src/service_install.rs` implements
`unpeel serve install|uninstall|status`, rendered from the verbatim-usable
templates in `packaging/service/` (launchd LaunchAgent + systemd `--user`
units; the templates document the exact anchor lines install rewrites).
Scope follows the serve rule: no `UNPEEL_HOME` → machine unit; a registered
workspace home (recovered by `workspaces::current_scope()` after
`--workspace` re-homed the process) → scoped `--workspace NAME serve` unit;
an unregistered home is refused. Constraints:

- **per-user only** (LaunchAgent / `systemctl --user`), never root — the
  service owns `~/.unpeel`, the Keychain, and the per-user machine lease.
  Docs must carry the macOS auto-login and Linux `loginctl enable-linger`
  notes.
- `uninstall` stops the managed service and removes only the unit file;
  workspace data and running Session PTYs are untouched.
- `launchctl`/`systemctl` are resolved via `PATH` and the unit directories
  derive from `HOME`/`XDG_CONFIG_HOME`, so tests drive real flows through
  shims (`tests/cases/serve_install.py`) and never register a real service.
  `UNPEEL_SERVICE_MANAGER=launchd|systemd` overrides platform detection for
  tests only.
- `status` reports the unit + service-manager view and the existing lease
  truth (`service::is_running` / `driver::is_running_at`), exit 0 only while
  the Host service is actually running.

### Browser engine (`unpeel browser install`)

`crates/unpeel-cli/src/browser_cli.rs` — the one Browser MCP verb; every
decision lives in `unpeel_core::browser_engine` so the CLI, the worker's
start-time install, and the MCP server can never disagree:

```text
unpeel browser install [--check] [--json]
```

Installs (or confirms) the pinned `agent-browser` under
`~/.unpeel/browser/bin` after sha256 verification against
`protocol/browser-engine-v1.json`; `--check` only reports and never
downloads. Exit codes: 0 engine ready · 1 download/hash/unsupported failure ·
3 (`--check`) missing or stale · 4 engine ready but no Chrome/Chromium on
this Host (the text names the binaries looked for; Unpeel never installs a
browser). `--json` prints `{state, version, path, error?, browser: {path |
null, error?}}`. Sample (macOS):

```text
engine:  agent-browser 0.34.0 — ready
path:    /Users/me/.unpeel/browser/bin/agent-browser
browser: /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
```

### Desktop-session service (`unpeel serve install --graphical`)

Linux only. Writes the `graphical-session.target`-bound variant of the
user unit (`packaging/service/unpeel-serve-graphical.service`) so the Host
runs inside the desktop session Computer Use needs; launchd refuses the
flag (the app owns the desktop daemon on macOS). `uninstall` and `status`
take no flag; `status` additionally prints `unit variant:`,
`graphical-session.target:` (`is-active`), and `desktop session:` — the
display plus session bus visible to the calling shell, or the missing
piece. Detail: `docs/agents/serve.md`.

### Computer Use engine (`unpeel computer install`)

`crates/unpeel-cli/src/computer_cli.rs` — the one Computer Use verb, the
same shape as `unpeel browser install`; every decision lives in
`unpeel_core::computer_engine` so the CLI, the worker's on-demand install,
and the MCP server can never disagree:

```text
unpeel computer install [--check] [--json]
```

Installs (or confirms) the pinned `cua-driver` under
`~/.unpeel/computer/bin` after two sha256 checks against
`protocol/computer-engine-v1.json`: the release tarball, then the one
extracted member (cua publishes tarballs, not bare binaries; the system
`tar` extracts exactly `archiveMember` from the already-verified archive).
`--check` only reports and never downloads. Both forms then run the engine
once (`--version`, bounded) so a binary that verified but cannot start is
never called ready. Exit codes: 0 engine ready · 1 download/hash/unsupported
failure, **or installed but cannot start** — missing X11 client libraries
are named with the apt line (`state: "failed"`) · 3 (`--check`) missing or
stale · 4 engine ready and runnable but no desktop session is visible to
this process (Linux:
the daemon needs the desktop's `DISPLAY`/`WAYLAND_DISPLAY` **and** a session
D-Bus for AT-SPI; the line names which is missing; on macOS the app owns the
daemon and the line reads `macOS (app-owned daemon)`). `--check` on an
override/bundled/PATH copy says so in the version field, since only the
managed copy is hash-verified. `--json` prints `{state, version, path,
error?, session: {display | null, error?}}`.
`UNPEEL_CUA_DRIVER_BIN=<path>` overrides the engine;
`UNPEEL_COMPUTER_ENGINE_INSTALL=0` keeps the Host service from installing
on demand (the verb still works by hand).

### Gates

At minimum, CLI settings/preset changes run:

```sh
(cd crates && cargo test -p unpeel-cli settings_cli::tests)
(cd crates && cargo test -p unpeel-cli --test settings_command)
(cd crates && cargo test -p unpeel-cli)
```

The real-process case proves JSON output, validation-before-write, nested and
top-level unknown-field preservation, preset flags/order, and that the state
bus notification is delivered before the process exits. Changes that touch
workspace selection must also run the full `crates/unpeel-cli/tests/run.sh`
matrix because every command composes with `--workspace`.
