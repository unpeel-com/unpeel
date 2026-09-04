# The shared PTY core

`unpeel-host __pty_core__` hosts **N Sessions in one process**. Each Session
still runs the unchanged `session_host::run_host` loop, just on its own thread
of the core instead of in its own `__session_host__` process. Everything a
Session publishes is byte-for-byte what a per-process Host publishes:
`manifest.json`, `session.sock`, `output.bin`, the attach protocol, the hook
env. Only the process boundary moves, so the app, the CLI, `unpeel-attach`,
serve, and the phone need no changes to consume a core-hosted Session.

Why: an empty per-process Host costs ~3.1 MiB phys_footprint (six threads,
malloc arenas, a VT grid). In the core the same empty `sh` Session costs
~0.6 MiB, and the process-level fixed cost is paid once (measured 2026-09-02:
core alone 1.2 MiB, 50 empty `sh` Sessions 32 MiB total).

Code: `crates/unpeel-core/src/pty_core.rs` (core process + client),
`session_host::spawn_host_process_from_launch_file` (routing),
`unpeel-host/src/main.rs` (argv dispatch).

## Contract

- **Process shape.** Started detached (setsid, stdio null) exactly like a
  session host, so it outlives the app and the serve worker. The core never
  exits on its own while it hosts a Session.
- **One instance per home.** flock on `$UNPEEL_HOME/pty-core.lock`; a second
  instance exits 0 immediately, which keeps "start a core, then launch"
  idempotent for every launcher.
- **Record.** `$UNPEEL_HOME/pty-core.json` =
  `{"pid","pid_started_at","socket","host_build_id","protocol":1}`, written
  after bind, removed on clean exit. `pid_started_at` comes from
  `process_start_time_ms`; readers must verify it before trusting the pid.
- **Socket.** `$UNPEEL_HOME/pty-core.sock`, mode 0600, one request per
  connection, newline-delimited JSON:
  - `{"op":"ping"}` → `{"ok":true,"pid":N,"sessions":K,"host_build_id":"…"}`
  - `{"op":"launch","launch_file":"/abs/path"}` →
    `{"ok":true,"session_id":"…"}` **only after the Session's preliminary
    manifest is on disk** (the same moment a per-process Host has it, so
    `unpeel-attach`'s ~2 s manifest wait still holds), or
    `{"ok":false,"error":"…"}`. The core reads and deletes the launch file
    exactly like `run_from_args`; a relaunch of an id it already hosts is
    refused.
  - `{"op":"shutdown"}` → exits only with zero hosted Sessions; otherwise
    `{"ok":false,"error":"busy","sessions":K}`. **Nothing may ever stop a core
    that hosts live Sessions.**
- **Routing.** `spawn_host_process_from_launch_file` (the one choke point
  every launcher reaches through `unpeel-host <launch-file>`) tries the core
  when `pty-core.sock` exists and `UNPEEL_PTY_CORE` is not `0` (connect
  timeout 2 s, launch reply timeout 10 s). On any failure it logs a
  `pty-core launch fallback` line to `hooks/trace.log` and spawns today's
  per-process Host. `UNPEEL_PTY_CORE=0` forces per-process hosting
  everywhere. Starting the core is the serve worker's job (the default
  since 0.4.4; only `UNPEEL_PTY_CORE=0` opts out, matching routing); the
  core itself does not start anything.
- **Failure isolation.** Each Session thread runs under `catch_unwind`. A
  panic or an `Err` from `run_host` marks that Session's manifest exited and
  logs to `trace.log`; sibling Sessions are untouched.

## The preliminary-manifest hazard

`run_host` writes a preliminary manifest before the PTY child exists. A
per-process Host uses **its own pid** as the liveness placeholder there and
replaces it with the child pid after spawn. Inside the core that placeholder
would be the **core's pid**, and any kill/normalize path that trusted it
(`reap_dead_sessions`, Remove, Reload) would group-kill every hosted Session.

So in core mode the preliminary manifest carries `pid: null` plus the additive
`host_pid` / `host_pid_started_at` fields (the Host process, core or
per-process; per-process Hosts publish them too). `manifest.pid` is still the
child pid once spawned, as today.

Rules for readers:

- **`pid` is the only kill target.** `host_pid` is liveness evidence for the
  launch window, never something to signal.
- A Running manifest with `pid: null` is alive iff
  `manifest_launching_host_is_alive` says so (host pid exists and its start
  time matches `host_pid_started_at`); use `manifest_is_live` for the whole
  liveness question. Rust readers already do: `manifest_host_is_healthy`,
  `refresh_manifest_health`, both phases of `reap_dead_sessions`, and
  `manifest_child_is_definitively_gone` (which never treats a pid-less record
  as gone).
- Readers that key liveness solely on `manifest.pid` show a core-hosted
  Session as stopped for the sub-second launch window and, on the kill side,
  do nothing (there is no pid) — safe, just briefly pessimistic. At the time
  of writing that is `crates/unpeel-serve/src/sessions.rs` (`running =`,
  one-liner: use `manifest_is_live`) and the Swift `killAndCleanup` /
  `replacementRestartAllowsState` paths, which fail closed on a nil pid.

## Fallback edge

If the core consumed the launch file but its reply is lost or late (the
client's 10 s wait expired), the client does **not** fall back — the file's
absence proves the core owns the launch and a per-process spawn would
double-host it. It logs `pty-core launch reply lost` and returns success;
the Session is already coming up in the core. The first launch after a core
starts can be slow because the preliminary manifest resolves App branding
through the installed-App index, whose first build probes the login shell's
PATH; the core warms that index at start so later launches reply in
milliseconds (per-process Hosts pay that probe on every launch).

## Inside the core: the event-driven session loop (Round 2)

A hosted Session owns **no thread**. After `run_host`'s setup (launch prep,
hook install, manifests, provider setup, PTY spawn) the Session is handed
to three core-wide services in `session_host::core_reactor` and
`session_host::session_io` (child modules of `session_host`):

- **One reactor thread** (`core-reactor`; kqueue on macOS/BSD, epoll on
  Linux, a ~150-line level-triggered poller over raw `libc`, no async
  runtime). It owns every PTY master read, every `session.sock` accept, and
  every long-lived attach client (`StreamOutput`, `StreamInput`), all
  non-blocking. One bounded read per ready PTY per pass keeps a flooding
  Session from starving the others; a client that cannot take bytes keeps
  them in its own outbox (the released 8 MiB backlog cap and 60 s stall
  drop still apply) and never blocks anyone. Each Session callback runs
  under `catch_unwind`: a panic ends that Session (exited manifest, error to
  its owner) and the reactor keeps serving the rest.
- **One timer thread** (`core-timer`) running every Session's heartbeat,
  menu/screen scan, and runtime-observer jobs with the released cadences;
  one Session's jobs still run strictly sequentially.
- **One journal writer** (`core-journal`) batching every Session's
  `output.bin` writes (same 32 ms / 128 KiB rules). Bytes in flight are
  counted per Session; above 4 MiB the reactor pauses that PTY's reads and
  resumes below 1 MiB, so a slow disk bounds memory instead of growing it.

One-shot control commands (Write, Resize, Ping, Kill, Resume/RestartAgent,
ViewportSnapshot) keep the released blocking handler and run on a
short-lived thread per request; the reactor reads the JSON line, then hands
the socket over. The `session.sock` protocol, `output.bin` semantics, hook
env, and manifest fields are unchanged.

**Per-session input isolation.** The PTY master is `O_NONBLOCK` and the
reactor is the only thread that writes it, always through that Session's
bounded input queue (`SessionIo::drain_pending_input`: write what the
kernel takes now, arm the writable event for the rest, 1 MiB cap per
Session). `StreamInput` frames, one-shot `Write` commands
(`Control::Input`, submitted from the command's transient thread and
answered once queued), and the Host's own terminal-query answers (DA1,
CPR, OSC colour) all go through it; nothing on the reactor thread ever
blocks on a PTY, and no transient thread holds a lock the reactor needs
across a blocking write (the reactor never takes `HostRuntime`'s lock for
input). A terminal that stops reading fills only its own queue: further
`Write`s to it are refused with `terminal is not accepting input`, a
flooding stream client is dropped, query answers that do not fit are
logged and dropped, and every sibling's `session.sock` keeps answering.
The one remaining direct writer, the in-place agent relaunch, uses
`PtyWriter` with a 5 s bound and holds `agent_restart_lock` meanwhile;
input queued for that Session during the relaunch is deferred (never
written between the stop and the new command) and drained on the
relaunch's wake or the next idle tick. The `Write` idempotency ledger
(`recent_write_ids`) lives on the reactor-owned `SessionIo`, so check →
queue → record is one thread's sequence.

**Slot identity.** A hosted Session is addressed by a `SlotRef` (slot +
monotonic generation). Every cross-thread message about a Session
(`Wake`, `Input`, `JournalDrained`, `JournalFailed`, `SessionEnded`,
`TimerMsg::Add`/`Remove`) carries one, and the reactor and timer drop any
whose generation is not the slot's current one. A slot leaves `sessions`
at `end_session` but returns to `free_slots` only when the teardown
thread reports `SessionEnded`, so a late timer retirement or journal
failure from a Session that already left can never reach the slot's next
occupant.

Session end (PTY EOF/error, Kill followed by a final drain, or the child's
exit observed by the reactor's process watch) detaches the fds on the
reactor and runs the old epilogue on a `session-exit` thread: journal
flushed and closed, timer jobs retired, **the child reaped**
(`reap_hosted_child`: `try_wait`, then SIGTERM, a 1.5 s grace, SIGKILL, a
bounded wait — signals only to a pid whose kernel start time matches the
recorded one), the real exit code and exited manifest under the manifest
lock, sockets removed, then the owner's `on_exit`. The core therefore never
accumulates `<defunct>` children, however a Session ended. Every child is
watched for exit at registration (`EVFILT_PROC` on macOS; no watch on
Linux yet, where the exit shows as PTY EOF), so a shell that exits while a
background job still holds the slave ends its Session instead of lingering
with a zombie. The per-process `__session_host__` runs the same services
with N = 1 and just blocks its main thread on that callback. After a
teardown the reactor's next idle tick hands freed heap back to the OS
(`malloc_zone_pressure_relief` / `malloc_trim`), so the core shrinks again
after `unpeel rm`.

Gates for the isolation and identity rules: `cargo test -p unpeel-core
core_reactor` (bounded queue that never blocks, generation-checked timer
and control messages with the slot held until the teardown reports, reaping
of a live child, child exit observed without EOF) and the PTY case
`pty_core_isolation` (a query-flooding terminal and a raw-mode child that
never reads next to a healthy sibling whose socket must answer within its
timeout; write-id dedup; kill and child-exit paths leaving no zombie and a
real exit code).

Measured 2026-09-02 (release, this Mac, `scripts/bench-memory.sh` with
`UNPEEL_PTY_CORE=1`): per empty `sh` Session 0.42 MiB (0.56 before), per
attached client ~0 KiB (102 KiB before), core alone with 4 threads
regardless of Session count. The remaining per-Session bytes are heap:
~258 KiB of small-object malloc and ~176 KiB in the VT's own pages
(`vmmap` at 1 vs 51 Sessions), which is the VT budget work, not threads.

## Idle diet: what a quiet Session keeps

A Session that has printed nothing for a second holds only its state, not
its buffers (`session_io::release_idle_buffers` on the reactor's idle tick,
`JournalSession::release_idle_capacity` on the writer's): the journal batch
buffer, the broadcaster's recent ring (only while no client is subscribed),
drained client outboxes, and the PTY input queue all give their capacity
back and re-grow from the next byte. A late subscriber that would have been
served from the released ring is refused and falls back to the journal,
exactly as one behind a full ring is today. Measured 2026-09-03 (release,
core on, `scripts/bench-memory.sh`): per empty Session 0.34 MiB, per filled
10k-line Session 0.62 → 0.44 MiB, and 0.39 MiB after ten quiet seconds
(the new (c') row, `BENCH_IDLE_WAIT`). The per-Session malloc heap at 1 vs
51 idle Sessions is ~248 KiB, of which ~224 KiB is libghostty-vt's 7 KiB
page-aligned allocations (`posix_memalign`, the VT grid); the reactor,
broadcaster, journal, and command state together are ~17 KiB.

## Handoff: upgrading the core without restarting a terminal

`unpeel-host __pty_core__ --takeover` replaces the running core in place.
Nothing on disk changes and no socket closes: the new core receives every
kernel object the old one held, over one connection on `pty-core.sock`,
with `SCM_RIGHTS` (`session_host::fd_pass`, raw `libc`):

1. new → old: the JSON line `{"op":"handoff","build_id":"…"}`. From here
   the connection is framed (4-byte length + payload, fds on the header).
2. old → new: `{"ok":true,"sessions":N,"protocol":1}` carrying the
   `pty-core.lock` fd (the flock travels with the open file description, so
   no third core can bind in between) and the `pty-core.sock` listener.
3. per Session, old → new: a `SessionHandoff` JSON line (id, cwd, command,
   shell, title/written flags, runtime generations, pty size, child pid +
   start time, dark mode, `journal_next_offset`, `retained_from`, pending
   PTY input, and each client's kind, `answers_queries`, offset, unsent
   `outbuf` and unconsumed `inbuf`), then the snapshot VT bytes
   (`TerminalViewportState::snapshot_vt`, the same formatter output
   `unpeel-attach` applies), with fds `[pty master, session.sock listener,
   clients…]`.
4. new → old: `{"ok":true}` after every Session is registered on its reactor
   and `pty-core.json` names the new pid; or `{"ok":false,"error":…}`.
5. old: on `ok` it forgets the moved fds (never running their destructors:
   portable-pty's writer would type EOF into the terminal), lets Sessions
   that were already tearing down finish, and exits 0 removing nothing. On
   an error, or the connection dropping first, it re-enables PTY reads and
   re-adds the timer jobs and keeps serving; a failed takeover is a no-op
   for every client.

Before exporting, the old core quiesces on its reactor thread: journal
`Flush` + ack per Session, timer jobs retired + ack, PTY read interest off,
streaming clients drained as far as their sockets accept. That is the only
pause a viewer can notice (milliseconds). The snapshot offset equals the
broadcaster's `next_offset` and the journal length, which the new core
asserts when it reopens `output.bin` (`RetainedOutputWriter::reopen` — no
truncation, unlike the replacement-Host constructor).

The new core hosts each Session with a `RawMasterPty` (ioctl/tcgetpgrp/dup
over the received fd) and a `HandedOverChild` (liveness by pid + kernel start
time; on macOS the reactor watches the process with `EVFILT_PROC` so the
exit code stays exact; on Linux the shell's exit shows as PTY EOF and
`exit_code` is recorded as unknown). Streaming clients are re-subscribed at
the handed-over offset with their carried bytes queued first, so a viewer
sees every byte once. `manifest.host_pid` is updated to the new core; `pid`
stays the child.

Refusals: `busy` while a launch's setup thread has not registered (the
handoff waits up to 15 s for it), `handing_off` for a second concurrent
request, `shutting_down`. Launches and `shutdown` arriving during a handoff
answer `handing_off`.

**Serve supervisor.** With `UNPEEL_PTY_CORE=1`, an adopted core whose
`host_build_id` differs from the binary the worker would launch
(`session_host::host_build_id_for(resolve_host_binary())`) gets exactly one
`--takeover` spawn per (old pid, expected build). `serve.json.ptyCore.state`
reads `handing_off` (with `takeoverFrom`), then `live` once the record names
the new pid and it answers `ping`. A takeover child that exits, or does not
publish within 30 s, returns the state to `adopted`; neither core is ever
signalled.

Gates: `cargo test -p unpeel-core --lib fd_pass`, `cargo test -p unpeel-serve
--lib pty_core_supervisor`, and the PTY case `pty_core_handoff` (five
Sessions with output in flight and an attached stream client, byte-equal
screens, journal continuity, the client still streaming, old core exited).

## Operating it by hand

```sh
export UNPEEL_HOME=$HOME/some-short-home       # socket paths must stay short
nohup unpeel-host __pty_core__ </dev/null >/dev/null 2>&1 &
echo '{"op":"ping"}'     | nc -U $UNPEEL_HOME/pty-core.sock
unpeel new …                                   # routes to the core
echo '{"op":"shutdown"}' | nc -U $UNPEEL_HOME/pty-core.sock   # busy until 0 sessions
```

Gates: `cargo test -p unpeel-core pty_core`, the PTY case
`crates/unpeel-cli/tests/run.sh pty_core_parity`, and
`UNPEEL_HOME=<home-with-a-core> scripts/verify-attach.sh`.

## Memory gate

`scripts/bench-memory.sh` is the source of truth for the per-terminal
numbers (`unpeel-apple:docs/plans/pty-core.md` "Measurement recipe", private); `UNPEEL_PTY_CORE=1`
measures the core. CI runs it on macOS with the core off and on and on
Ubuntu headless (`.github/workflows/bench-memory.yml`), publishes each table
to the job summary, and fails when reclamation is not total or a row exceeds
its ceiling in `scripts/bench-thresholds.json`. Tighten ceilings when a
round lands; never loosen one without a note in that file. The in-process
cost of one libghostty-vt grid is measured by the ignored
`vt_footprint_per_terminal` test in `terminal_viewport.rs` (run it in
release with `--ignored --nocapture`).
