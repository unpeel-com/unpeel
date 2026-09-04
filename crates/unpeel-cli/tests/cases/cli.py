"""The headless CLI. Every verb the TUI has is also a command, so scripts
and agents can drive Unpeel with no UI — and `wait` returns a real exit
code so it composes into a pipeline."""

import subprocess
import sys, os, json, time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli, wait_running  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="cat", command="cat")

    started = run_cli(home, ["new", "--preset", "cat", "--project", "p"])
    case.check("new starts a session", started.returncode == 0, started.stderr[:200])
    session_id = ""
    for line in started.stdout.split():
        if len(line) == 36 and line.count("-") == 4:
            session_id = line
    case.check("new prints the session id", bool(session_id), started.stdout[:200])
    short = session_id[:8]

    case.check(
        "the host comes up",
        wait_running(home, session_id),
        "manifest never reached state=running",
    )

    listed = run_cli(home, ["ls", "--json"])
    try:
        rows = json.loads(listed.stdout)
    except ValueError:
        rows = []
    case.check(
        "ls --json is machine readable",
        any(row["id"] == session_id and row["command"] == "cat" for row in rows),
        listed.stdout[:200],
    )

    case.check(
        "send accepts an id prefix",
        run_cli(home, ["send", short, "hello-from-cli", "--enter"]).returncode == 0,
    )
    time.sleep(1.5)
    screen = run_cli(home, ["screen", short])
    case.check("screen shows what the session printed", "hello-from-cli" in screen.stdout,
               screen.stdout[:200])

    logs = run_cli(home, ["logs", short])
    case.check("logs tails the output", "hello-from-cli" in logs.stdout)

    # An escape that leaves the terminal alive. Ctrl-C here reaches the
    # `-c` wrapper shell while `cat` is its foreground job and aborts the
    # rest of the startup script, so the Session exits (on every Host); the
    # old per-session reader only noticed the EOF ~300 ms later, which let
    # the "live terminal" check below slip in first.
    case.check("keys accepts escapes", run_cli(home, ["keys", short, "\\x15"]).returncode == 0)

    start = time.time()
    waited = run_cli(home, ["wait", short, "--idle"], timeout=40)
    case.check(
        "wait --idle exits 0 when it settles",
        waited.returncode == 0 and (time.time() - start) < 25,
        f"rc={waited.returncode} after {time.time() - start:.1f}s",
    )

    denied = run_cli(home, ["resume", short], timeout=20)
    case.check(
        "resume refuses a live terminal without a returned managed agent",
        denied.returncode != 0
        and "no managed agent to resume" in denied.stderr
        and session_id in home.manifests(),
        denied.stdout[:120] + denied.stderr[:200],
    )

    case.check(
        "archive writes the shared marker",
        run_cli(home, ["archive", short]).returncode == 0
        and home.has_marker(session_id, "archived.json"),
    )
    case.check(
        "restore clears it",
        run_cli(home, ["restore", short]).returncode == 0
        and not home.has_marker(session_id, "archived.json"),
    )

    # A current live managed Host whose agent returned to the shell receives
    # Resume Agent and keeps the same Session/PTY identity.
    managed_id = "cli-managed"
    home.session(
        managed_id,
        label="managed CLI session",
        command="claude",
        project_id="p",
        running=True,
        extra_manifest={"host_protocol_version": 3},
    )
    home.seed_resume_data(managed_id)
    managed_host = case.host(managed_id)
    resumed_agent = run_cli(home, ["resume", managed_id], timeout=20)
    case.check(
        "Resume Agent keeps a live managed session in place",
        resumed_agent.returncode == 0
        and resumed_agent.stdout.strip() == managed_id
        and managed_host.resume_agent_generations == [0]
        and home.manifests().get(managed_id, {}).get("state") == "running",
        str((resumed_agent.stdout, resumed_agent.stderr,
             managed_host.resume_agent_generations,
             home.manifests().get(managed_id))),
    )

    active_id = "cli-active"
    home.session(
        active_id,
        label="active managed CLI session",
        command="claude",
        project_id="p",
        running=True,
        extra_manifest={
            "host_protocol_version": 3,
            "runtime": {"currentObservation": {"id": "claude"}},
        },
    )
    case.host(active_id)
    active_resume = run_cli(home, ["resume", active_id], timeout=20)
    case.check(
        "Resume Agent refuses to interrupt an active managed runtime",
        active_resume.returncode != 0
        and "managed agent is still active" in active_resume.stderr,
        active_resume.stdout[:120] + active_resume.stderr[:200],
    )

    # A manifest left as running after its Host dies is normalized to stopped
    # by discovery and gets ordinary replacement Resume, not Resume Agent.
    crashed_id = "cli-crashed-host"
    # A REAL crash signature: a recorded child pid that provably no longer
    # exists. A running record with no pid at all is unknowable and fails
    # closed (never resumable), per the pid-identity rules.
    dead_child = subprocess.Popen(["true"])
    dead_child.wait(timeout=10)
    home.session(
        crashed_id,
        label="crashed Host session",
        command="claude",
        project_id="p",
        state="running",
        running=False,
        extra_manifest={"host_protocol_version": 3, "pid": dead_child.pid},
    )
    home.seed_resume_data(crashed_id)
    crashed_resume = run_cli(home, ["resume", crashed_id], timeout=45)
    crashed_new_id = (
        crashed_resume.stdout.strip().split()[-1]
        if crashed_resume.stdout.strip()
        else ""
    )
    case.check(
        "a crashed Host exposes ordinary replacement Resume",
        crashed_resume.returncode == 0
        and len(crashed_new_id) == 36
        and crashed_new_id != crashed_id,
        crashed_resume.stdout[:200] + crashed_resume.stderr[:200],
    )

    # A stopped blank terminal has no agent to preserve, so Resume keeps the
    # shipped terminal-replacement behavior and returns its new Session id.
    stopped_id = "cli-stopped-blank"
    home.session(stopped_id, label="stopped blank", command="", project_id="p")
    resumed = run_cli(home, ["resume", stopped_id], timeout=45)
    new_id = resumed.stdout.strip().split()[-1] if resumed.stdout.strip() else ""
    case.check(
        "resume replaces a stopped terminal",
        resumed.returncode == 0 and len(new_id) == 36 and new_id != stopped_id,
        resumed.stdout[:200] + resumed.stderr[:160],
    )

    case.check("projects list works", run_cli(home, ["projects", "list"]).returncode == 0)
    case.check("presets list works", "cat" in run_cli(home, ["presets", "list"]).stdout)

    missing = run_cli(home, ["screen", "nope-nope"])
    case.check(
        "an unknown id is an error, not a guess",
        missing.returncode == 1 and "no session matching" in missing.stderr,
        missing.stderr[:160],
    )

    removed = run_cli(home, ["rm", new_id], timeout=45)
    case.check(
        "rm deletes the session directory",
        removed.returncode == 0 and not os.path.exists(home.path("app-sessions", new_id)),
    )

    # `unpeel add` registers the working directory as a project.
    scratch = home.path("a-project")
    os.makedirs(scratch, exist_ok=True)
    env_before = len(home.state()["projects"])
    added = run_cli(home, ["add", scratch])
    case.check(
        "add registers a folder as a project",
        added.returncode == 0 and len(home.state()["projects"]) == env_before + 1,
        added.stdout[:160] + added.stderr[:160],
    )


run("cli", body)
