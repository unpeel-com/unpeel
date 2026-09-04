# CLI + Host service tests

End-to-end coverage for `unpeel`: each case drives the **real binaries**
(`unpeel serve`, the one-shot verbs, `unpeel-host`) against a **private
`UNPEEL_HOME`**, and asserts on what lands on disk, what the Host publishes
over `/mobile`, `host.sock`, and the relay, and what a script sees.

```sh
./run.sh                  # everything (~10 min on a development Mac)
./run.sh pairing state    # cases whose name contains a filter
./run.sh -v compat_serve  # stream a case's own output
UNPEEL_TUI_SKIP_BUILD=1 ./run.sh   # skip the cargo build
```

`cargo test -p unpeel-cli` runs the unit tests and the in-process
`serve`/`settings` integration tests. The suites here are opt-in from cargo:

```sh
UNPEEL_TUI_PTY_TESTS=1 cargo test -p unpeel-cli --test pty_suites
```

(The `UNPEEL_TUI_*` variable names predate the removal of the terminal UI
and are kept so existing scripts and CI keep working: `UNPEEL_TUI_BINARY`,
`UNPEEL_TUI_SKIP_BUILD`, `UNPEEL_TUI_TEST_BASE`, `UNPEEL_TUI_TEST_HOME`,
`UNPEEL_TUI_PTY_TESTS`.)

## Why it is built this way

**Fixtures are per-case and built from scratch.** Cases used to share one
`UNPEEL_HOME` and mutate each other's state, which produced both false passes
and false failures depending on run order. `Case` creates and destroys its
own home, and the runner sweeps any hosted process or PTY core a case leaves
behind before deleting it.

**Assertions poll.** Hosts spawn, the worker rescans at 1 s, the relay
connects asynchronously. `Serve.wait_for(predicate)` and
`Pty.expect(...)` poll until the condition holds, so a slow machine is slow
rather than red.

**The home path must be short.** A hosted session binds
`<home>/app-sessions/<uuid>/session.sock`; `sockaddr_un` caps the path near
104 bytes. macOS `TMPDIR` alone is ~49, which silently breaks every host
spawn. The runner uses `/tmp/ut-<case>` and the harness refuses a home that
would overflow. Never run two `run.sh` invocations concurrently on one
machine: cases bind ports and spawn hosted processes.

## Writing a case

```python
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli

def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.session("s1", label="a session", project_id="p", running=True)

    service = case.serve()
    case.check("the Host comes up", bool(service.ready()))
    listed = run_cli(home, ["ls"])
    case.check("the session is listed", "a session" in listed.stdout)

run("my-case", body)
```

Useful pieces:

| | |
|---|---|
| `case.serve(env=…)` | a foreground scoped `unpeel serve` (`ready`, `wait_for`, `read_for`, `status`, `log`, `close`) |
| `run_cli(home, [...])` | the one-shot CLI against the same home |
| `case.pty(args=("pair",))` | a real PTY for verbs that draw (the pairing QR); `expect`, `send`, `grid` |
| `case.app()` | a mock desktop app: owns a port, answers `/mcp/*`, records `calls`; `fail_routes=` simulates an **older** app |
| `case.host(id)` | a fake session host on the control socket — records writes, resizes, and agent-restart generations |
| `home.session(...)` | a hosted-session dir; `running=True` parks a real pid and binds a socket |
| `home.marker(...)` | shared markers (`archived.json`, `title.json`, `read.json`) |
| `home.pair_device()` / `home.reserve_mobile_port()` | a paired phone token and the published phone endpoint |
| `mobile_request(port, path, token)` | an authenticated `/mobile/*` call over TLS, pinned to the Host certificate the way a phone pins (`host_certificate_pin(home)`); `plaintext_mobile_request` is the cleartext shape the Host must refuse with 426 |
| `McpClient(home, session_id)` | a `unpeel-host __mcp__` child speaking JSON-RPC for one caller Session (`tool_names`, `call`, `McpClient.text`) |

Name checks as the behaviour a user would recognise ("archive falls back to
the shared marker"), not the mechanism. Pass a `detail` argument — it is
printed only when the check fails.

## What is covered

| case | what it protects |
|---|---|
| `standalone` | `unpeel serve` is a complete persistent Host with no app: verbs, restart, lease |
| `mobile` | bootstrap, output, organization, polite-guest port rules for a paired phone |
| `pairing` | QR + pairing against the **shipped Swift crypto**, `unpeel pair` through the worker |
| `approvals` | MCP approvals owned by the Host, answerable from a phone |
| `host_launch_conformance` | native and headless launchers enter the same canonical Host runtime (71 conformance cases) |
| `cli` | headless verbs, including live agent-only restart vs stopped terminal Resume |
| `browser_spawn` | Browser MCP engine launch from a Session |
| `ctrlc_fallback` | Ctrl-C into a fresh preset Session lands in the fallback shell |
| `pty_core_*` | the shared PTY core: parity, adoption, in-place handoff |
| `serve_install` | `unpeel serve install|uninstall|status` unit rendering (shim-tested) |
| `state_bus` | cross-frontend sync: pings land immediately, writes announce |
| `link_enroll` / `link_lifecycle` / `link_token_rotation` | scripted Link enrollment; the live-serve refresh/reject/race ladder; relay credential rotation |
| `relay_conformance` | the Host's relay uplink against the shipped known-answer vectors |
| `compat_state` | **upgrade safety**: unmodelled keys, legacy/future manifests, corrupt files, through the Host and the CLI |
| `compat_bridge` | **version skew**: this CLI/Host beside an older app that 404s new routes |
| `compat_serve` | **version skew**: the shipped 0.4.3 `unpeel` beside this tree's binaries over one home, both directions |

The three `compat_*` cases exist because users update the app and the CLI
independently. Treat a failure there as "this would break someone's
install", not as a test needing adjustment. `compat_serve` needs the pinned
archive: it is cached under `~/Library/Caches/unpeel-matrix/<version>/`
(sha256 pinned in the case), `UNPEEL_MATRIX_COMPAT_ARCHIVE=<path>` overrides
for offline/CI runs, and when neither is available the case prints a
`NOTE … SKIPPED` line rather than passing silently.
