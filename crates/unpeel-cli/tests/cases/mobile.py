"""Serve a paired phone with no desktop app in the Host tree: `unpeel serve`
is the complete phone-facing Host."""

import sys, os, base64, hashlib, json, time, urllib.error, urllib.parse, urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, mobile_request  # noqa: E402


def mobile_binary_request(port, path, token, data, content_type, timeout=10):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=data, method="POST"
    )
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Content-Type", content_type)
    try:
        response = urllib.request.urlopen(request, timeout=timeout)
        return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b"{}")
        except ValueError:
            return error.code, {}
    except Exception as error:  # noqa: BLE001 - surfaced as a failed check
        return 0, {"error": str(error)}


def body(case):
    home = case.home
    home.project("proj-1", "unpeel", "/tmp")
    home.project("proj-empty", "Empty project", "/tmp")
    home.preset(label="Mobile cat", command="cat", preset_id="mobile-cat")
    state = home.state()
    state["projects"].append({
        "id": "group-research",
        "name": "Research",
        "path": "/tmp",
        "parent_project_id": "proj-1",
        "is_folder": True,
        "sort_order": 1,
    })
    with open(home.path("app-state.json"), "w") as handle:
        json.dump(state, handle, indent=2)
    home.session("fixture-1", label="fixture session", project_id="proj-1",
                 created_at=1_754_300_000_000)
    home.session("fixture-2", label="another session", project_id="proj-1",
                 created_at=1_754_400_000_000)
    home.session("fixture-org", label="organize me", project_id="proj-1",
                 created_at=1_754_450_000_000)
    home.session("fixture-live", label="live mobile session", command="claude",
                 project_id="proj-1", created_at=int(time.time() * 1000), running=True,
                 extra_manifest={"host_protocol_version": 3})
    live_host = case.host("fixture-live", cols=80, rows=24)
    home.session(
        "fixture-active",
        label="active managed session",
        command="claude",
        project_id="proj-1",
        created_at=int(time.time() * 1000),
        running=True,
        extra_manifest={
            "host_protocol_version": 3,
            "runtime": {"currentObservation": {"id": "claude"}},
        },
    )
    case.host("fixture-active")
    home.session("fixture-old-host", label="old Host managed session", command="claude",
                 project_id="proj-1", created_at=int(time.time() * 1000), running=True,
                 extra_manifest={"host_protocol_version": 2})
    case.host("fixture-old-host")
    home.session("fixture-slow", label="slow mobile session", command="cat",
                 project_id="proj-1", created_at=1_754_200_000_000, running=True)
    slow_host = case.host("fixture-slow", response_delay=3)
    # Resume-capability fixtures need real resume evidence (restart for the
    # exited row, Resume Agent for the returned live one).
    home.seed_resume_data("fixture-1", "fixture-2", "fixture-live")
    home.marker(
        "fixture-2",
        "project-override.json",
        {"project_id": "group-research", "moved_at": 1_754_400_000_100},
    )
    home.marker(
        "fixture-org",
        "project-override.json",
        {"project_id": "group-research", "moved_at": 1_754_450_000_100},
    )
    for session_id, label, created_at in [
        ("fixture-archive-old", "older archived session", 1_754_100_000_000),
        ("fixture-archive-new", "newer archived session", 1_754_600_000_000),
    ]:
        home.session(session_id, label=label, project_id="proj-1",
                     created_at=created_at)
        home.marker(session_id, "archived.json", {"archived_at": created_at})
    home.marker(
        "fixture-archive-new",
        "title.json",
        {"title": "renamed archived session"},
    )
    home.session("fixture-archive-group", label="group archived session",
                 project_id="proj-1", created_at=1_754_350_000_000)
    home.marker(
        "fixture-archive-group",
        "project-override.json",
        {"project_id": "group-research", "moved_at": 1_754_350_000_100},
    )
    home.marker(
        "fixture-archive-group",
        "archived.json",
        {"archived_at": 1_754_350_000_200},
    )
    screenshots = home.path("app-sessions", "fixture-1", "artifacts", "browser", "screenshots")
    os.makedirs(screenshots, exist_ok=True)
    screenshot_bytes = b"\x89PNG\r\nheadless-artifact"
    with open(os.path.join(screenshots, "result.png"), "wb") as handle:
        handle.write(screenshot_bytes)
    token = home.pair_device()
    with open(home.path("mobile", "mac-id"), "w") as handle:
        handle.write("fixture-mac-id\n")
    port = home.reserve_mobile_port()

    driver = case.serve()
    ready = driver.ready(timeout=15.0)
    case.check(
        "serve owns the headless Host",
        bool(ready)
        and ready.get("pid") == driver.pid
        and ready.get("nativeAppOwnsControllers") is False
        and ready.get("directPort") == port,
        str(ready or driver.log()),
    )
    driver.read_for(6.0)

    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("the phone server comes up app-lessly", status == 200, str(status))
    if status != 200:
        return

    status, boot = mobile_request(port, "/mobile/bootstrap", token)
    case.check("bootstrap answers", status == 200, str(status))
    case.check(
        "it binds the endpoint the app persisted",
        status == 200,
        "the phone keeps working across an app/Host handover",
    )
    case.check(
        "it identifies this Mac",
        boot.get("macID") == "fixture-mac-id" and boot.get("protocolVersion") == 1,
        str({k: boot.get(k) for k in ("macID", "protocolVersion")}),
    )
    host_capabilities = set(boot.get("hostProtocol", {}).get("capabilities", []))
    case.check(
        "bootstrap advertises the implemented headless Host operations",
        {
            "artifact.request_screenshot",
            "artifact.list",
            "artifact.read",
            "artifact.upload.resumable",
            "session.archive.list",
            "session.create",
            "session.order.set",
            "session.pin.set",
            "session.runtime.resume",
            "session.title.set",
            "session.transcript.markdown",
        }.issubset(host_capabilities)
        and "session.runtime.restart" not in host_capabilities
        and "session.notify_when_done.set" not in host_capabilities,
        str(sorted(host_capabilities)),
    )

    # Remote create resolves every executable/path from Host-owned project
    # and preset state. A stable request id replays the original receipt
    # without launching another PTY, and initial text is delivered only after
    # the detached Host's control socket is ready.
    create_before = set(home.manifests())
    create_headers = {"X-Unpeel-Request-ID": "mobile-create-once"}
    create_body = {
        "projectID": "proj-1",
        "presetID": "mobile-cat",
        "initialText": "hello from remote create",
        "initialTextSubmitMode": "pasteAndSubmit",
    }
    create_status, created = mobile_request(
        port, "/mobile/sessions", token, method="POST",
        body=create_body, timeout=15, headers=create_headers,
    )
    replay_status, replayed = mobile_request(
        port, "/mobile/sessions", token, method="POST",
        body=create_body, timeout=15, headers=create_headers,
    )
    created_id = created.get("sessionID")

    def created_session_is_live():
        manifest = home.manifests().get(created_id, {})
        output_path = home.path("app-sessions", created_id or "", "output.bin")
        try:
            output = open(output_path, "rb").read()
        except OSError:
            output = b""
        return (
            manifest.get("state") == "running"
            and b"hello from remote create" in output
        )

    create_live = driver.wait_for(created_session_is_live, timeout=12)
    create_after = home.manifests()
    new_create_ids = set(create_after).difference(create_before)
    created_manifest = create_after.get(created_id, {})
    case.check(
        "headless create launches one Host-owned preset and replays its receipt",
        create_status == 200
        and replay_status == 200
        and replayed == created
        and bool(created_id)
        and new_create_ids == {created_id}
        and created_manifest.get("session", {}).get("project_id") == "proj-1"
        and created_manifest.get("session", {}).get("command") == "cat"
        and created_manifest.get("cwd") == "/tmp"
        and bool(create_live),
        str((create_status, created, replay_status, replayed,
             new_create_ids, created_manifest)),
    )

    # ── phone fit is Host truth: published in the summary, cleared on demand ──
    # A desktop Controller (the Mac app in Host-service client mode) derives
    # its letterbox and "fit to desktop" control from these fields, and its
    # clear is the same `clear` verb the phone uses.
    def published_fit():
        _, boot_now = mobile_request(port, "/mobile/bootstrap", token)
        for session in boot_now.get("sessions", []):
            if session.get("id") == created_id:
                return (session.get("phoneFitColumns"), session.get("phoneFitRows"),
                        session.get("phoneFitSinceUnixMs"))
        return None

    fit_status, _ = mobile_request(
        port, "/mobile/resize-desktop", token, method="POST",
        body={"sessionID": created_id, "columns": 61, "rows": 23},
    )
    fit_seen = driver.wait_for(lambda: (published_fit() or (None,))[:2] == (61, 23), timeout=10)
    fit_marker = home.path("app-sessions", created_id or "", "phone-fit.json")
    case.check(
        "resize-desktop publishes the phone fit in the session summary",
        fit_status == 200 and bool(fit_seen)
        and (published_fit() or (None, None, None))[2] is not None
        and os.path.exists(fit_marker),
        str((fit_status, published_fit(), os.path.exists(fit_marker))),
    )
    clear_status, _ = mobile_request(
        port, "/mobile/resize-desktop", token, method="POST",
        body={"sessionID": created_id, "clear": True},
    )
    fit_gone = driver.wait_for(lambda: published_fit() == (None, None, None), timeout=10)
    case.check(
        "clear removes the published phone fit",
        clear_status == 200 and bool(fit_gone) and not os.path.exists(fit_marker),
        str((clear_status, published_fit(), os.path.exists(fit_marker))),
    )
    unknown_project_status, _ = mobile_request(
        port, "/mobile/sessions", token, method="POST",
        body={"projectID": "missing", "presetID": "mobile-cat"},
    )
    unknown_preset_status, _ = mobile_request(
        port, "/mobile/sessions", token, method="POST",
        body={"projectID": "proj-1", "presetID": "missing"},
    )
    case.check(
        "headless create rejects unknown or non-executable Host catalog entries",
        unknown_project_status == 400
        and unknown_preset_status == 400
        and set(home.manifests()).difference(create_before) == {created_id},
        str((unknown_project_status, unknown_preset_status)),
    )
    # A sidebar group is a folder record carrying its parent's path: creating
    # inside it is ordinary (0.4.3). The Session files under the group and
    # runs in the parent directory.
    group_create_status, group_created = mobile_request(
        port, "/mobile/sessions", token, method="POST",
        body={"projectID": "group-research", "presetID": "mobile-cat"},
    )
    group_session_id = group_created.get("sessionID")
    group_manifest = {}
    group_listed = {}
    # A per-process launch pays an interactive login-shell PATH probe before
    # its manifest settles (1-2 s idle on this Mac, far more under load), so
    # the listing can lag creation by well over ten seconds on a busy box.
    for _ in range(150):
        group_manifest = home.manifests().get(group_session_id or "", {})
        _, boot_after_group = mobile_request(port, "/mobile/bootstrap", token)
        group_listed = next(
            (item for item in boot_after_group.get("sessions", [])
             if item.get("id") == group_session_id), {})
        if group_manifest.get("cwd") and group_listed.get("projectID"):
            break
        time.sleep(0.2)
    case.check(
        "headless create launches inside a sidebar group in the parent directory",
        group_create_status == 200
        and bool(group_session_id)
        and group_manifest.get("cwd") == "/tmp"
        and group_listed.get("projectID") == "group-research",
        str((group_create_status, group_session_id, group_manifest.get("cwd"),
             group_listed.get("projectID"),
             {"listed": group_listed,
              "manifest": {k: group_manifest.get(k) for k in ("state", "pid", "host_pid")},
              "manifest_project": (group_manifest.get("session") or {}).get("project_id"),
              "bootstrap_ids": sorted(item.get("id", "")[:8] for item in boot_after_group.get("sessions", []))})),
    )

    sessions = {session["id"]: session for session in boot.get("sessions", [])}
    projects = {project["id"]: project for project in boot.get("projects", [])}
    case.check(
        "bootstrap sends groups as inline project children",
        projects.get("group-research", {}).get("isGroup") is True
        and projects["group-research"].get("parentProjectID") == "proj-1"
        and sessions.get("fixture-2", {}).get("projectID") == "group-research",
        str({
            "group": projects.get("group-research"),
            "session": sessions.get("fixture-2"),
        }),
    )
    case.check(
        "sessions carry what the phone needs",
        "fixture-1" in sessions
        and sessions["fixture-1"]["status"] == "exited"
        and sessions["fixture-1"]["title"] == "fixture session"
        and sessions["fixture-1"]["providerID"] == "claude"
        and sessions["fixture-1"]["capabilities"]["restart"] is True,
        str(sessions.get("fixture-1")),
    )
    case.check(
        "headless sessions do not offer notify-when-done without push delivery",
        sessions.get("fixture-org", {}).get("capabilities", {}).get("notifyWhenDone") is False,
        str(sessions.get("fixture-org")),
    )
    case.check(
        "returned managed agents split Resume Agent from exited Resume",
        sessions.get("fixture-live", {}).get("capabilities", {}).get("resumeAgent") is True
        and sessions.get("fixture-live", {}).get("capabilities", {}).get("restart") is False
        and sessions.get("fixture-active", {}).get("capabilities", {}).get("resumeAgent") is False
        and sessions.get("fixture-old-host", {}).get("capabilities", {}).get("resumeAgent") is False
        and sessions.get("fixture-old-host", {}).get("capabilities", {}).get("restart") is False
        and sessions.get("fixture-1", {}).get("capabilities", {}).get("resumeAgent") is False
        and all("restartAgent" not in session.get("capabilities", {}) for session in sessions.values()),
        str({
            "live": sessions.get("fixture-live"),
            "active": sessions.get("fixture-active"),
            "oldHost": sessions.get("fixture-old-host"),
            "exited": sessions.get("fixture-1"),
        }),
    )

    # Organization mutations use the shared title marker and app-state pin
    # contract. They must survive a bootstrap rebuild and retries must not
    # duplicate/reorder a pin.
    organized_title = "Headless organization"
    status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org", "title": organized_title, "pinned": True},
    )
    title_marker = home.read_marker("fixture-org", "title.json") or {}

    def organization_pins():
        grouped = home.state().get("pinned_sessions", {})
        return [
            (project_id, entry)
            for project_id, entries in grouped.items()
            for entry in entries
            if entry.get("session_id") == "fixture-org"
        ]

    pins_after_first = organization_pins()
    case.check(
        "organization writes title and effective-group pin to shared storage",
        status == 200
        and title_marker.get("title") == organized_title
        and len(pins_after_first) == 1
        and pins_after_first[0][0] == "group-research",
        str((status, title_marker, pins_after_first)),
    )
    first_pinned_at = (
        pins_after_first[0][1].get("pinned_at") if pins_after_first else None
    )
    retry_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org", "pinned": True},
    )
    pins_after_retry = organization_pins()
    case.check(
        "organization pin retries are idempotent and retain ordering",
        retry_status == 200
        and len(pins_after_retry) == 1
        and pins_after_retry[0][1].get("pinned_at") == first_pinned_at,
        str((retry_status, first_pinned_at, pins_after_retry)),
    )

    organized_boot = {}

    def bootstrap_contains_organization():
        nonlocal organized_boot
        boot_status, organized_boot = mobile_request(port, "/mobile/bootstrap", token)
        summary = next(
            (item for item in organized_boot.get("sessions", [])
             if item.get("id") == "fixture-org"),
            {},
        )
        return (
            boot_status == 200
            and summary.get("title") == organized_title
            and summary.get("pinned") is True
        )

    organization_published = driver.wait_for(bootstrap_contains_organization, timeout=12)
    case.check(
        "organization side effects publish through the next bootstrap",
        bool(organization_published),
        str(organized_boot),
    )

    unsupported_status, unsupported = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={
            "sessionID": "fixture-org",
            "title": "must not partially apply",
            "notifyWhenDone": True,
        },
    )
    case.check(
        "notify-when-done is explicitly unsupported and the patch is atomic",
        unsupported_status == 501
        and "not supported" in unsupported.get("error", "")
        and (home.read_marker("fixture-org", "title.json") or {}).get("title")
        == organized_title,
        str((unsupported_status, unsupported)),
    )
    empty_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org"},
    )
    whitespace_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org", "title": "  \n  "},
    )
    invalid_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org", "pinned": "yes"},
    )
    invalid_notify_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org", "notifyWhenDone": "yes"},
    )
    unknown_empty_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "missing-organization-session"},
    )
    unknown_notify_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={
            "sessionID": "missing-organization-session",
            "notifyWhenDone": True,
        },
    )
    case.check(
        "organization matches native validation and resource-order semantics",
        empty_status == 200
        and whitespace_status == 200
        and invalid_status == 400
        and invalid_notify_status == 400
        and unknown_empty_status == 404
        and unknown_notify_status == 404
        and (home.read_marker("fixture-org", "title.json") or {}).get("title")
        == organized_title,
        str((
            empty_status,
            whitespace_status,
            invalid_status,
            invalid_notify_status,
            unknown_empty_status,
            unknown_notify_status,
        )),
    )
    unpin_status, _ = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={"sessionID": "fixture-org", "pinned": False},
    )
    case.check(
        "organization unpin removes every shared pin entry",
        unpin_status == 200 and organization_pins() == [],
        str((unpin_status, organization_pins())),
    )

    # Hand-ordered sidebar ranks from a phone land in the shared
    # session-order.json through the same locked write + announce a desktop
    # drag commits through, and validation matches the native adapter.
    order_status, _ = mobile_request(
        port, "/mobile/session-order", token, method="POST",
        body={"projectID": "group-research",
              "orderedSessionIDs": ["fixture-org", "fixture-2"]},
    )
    order_file = {}
    try:
        with open(home.path("session-order.json")) as handle:
            order_file = json.load(handle)
    except OSError:
        pass
    order_missing_project_status, _ = mobile_request(
        port, "/mobile/session-order", token, method="POST",
        body={"orderedSessionIDs": ["fixture-org"]},
    )
    order_malformed_ids_status, _ = mobile_request(
        port, "/mobile/session-order", token, method="POST",
        body={"projectID": "group-research", "orderedSessionIDs": "fixture-org"},
    )
    case.check(
        "session order writes shared ranks and validates like native",
        order_status == 200
        and order_file.get("group-research") == ["fixture-org", "fixture-2"]
        and order_missing_project_status == 400
        and order_malformed_ids_status == 400,
        str((
            order_status,
            order_file,
            order_missing_project_status,
            order_malformed_ids_status,
        )),
    )

    # Pin persistence is the first compound effect. A broken shared state
    # file must fail before either the title marker or archive marker changes.
    valid_state = home.state()
    malformed_state = dict(valid_state)
    malformed_state["pinned_sessions"] = []
    home.write_state(malformed_state)
    failed_status, failed = mobile_request(
        port, "/mobile/session-organization", token, method="POST",
        body={
            "sessionID": "fixture-org",
            "title": "must not land after pin failure",
            "pinned": True,
            "archived": True,
        },
    )
    title_after_failure = home.read_marker("fixture-org", "title.json") or {}
    archived_after_failure = home.read_marker("fixture-org", "archived.json")
    home.write_state(valid_state)
    case.check(
        "compound organization preflights pin storage before title/archive",
        failed_status == 500
        and "pin preflight failed" in failed.get("error", "")
        and title_after_failure.get("title") == organized_title
        and archived_after_failure is None,
        str((failed_status, failed, title_after_failure, archived_after_failure)),
    )
    case.check(
        "bootstrap mirrors recent archived sidebar rows and project-scoped counts",
        sessions.get("fixture-archive-old", {}).get("archived") is True
        and sessions.get("fixture-archive-new", {}).get("archived") is True
        and sessions.get("fixture-archive-group", {}).get("archived") is True
        and projects.get("proj-1", {}).get("archivedSessionCount") == 2
        and projects.get("group-research", {}).get("archivedSessionCount") == 1,
        str({
            "sessionIDs": sorted(sessions),
            "project": projects.get("proj-1"),
            "group": projects.get("group-research"),
        }),
    )

    status, archive = mobile_request(
        port, "/mobile/archive?project_id=proj-1", token
    )
    archived_sessions = archive.get("sessions", [])
    case.check(
        "headless archive lists full summaries newest first",
        status == 200
        and archive.get("projectID") == "proj-1"
        and [item.get("id") for item in archived_sessions]
        == ["fixture-archive-new", "fixture-archive-old"]
        and archived_sessions[0].get("title") == "renamed archived session"
        and all(item.get("projectID") == "proj-1"
                and item.get("archived") is True
                and item.get("providerID") == "claude"
                for item in archived_sessions),
        str((status, archive)),
    )
    status, group_archive = mobile_request(
        port, "/mobile/archive?project_id=group-research", token
    )
    case.check(
        "archive grouping follows the effective group override",
        status == 200
        and [item.get("id") for item in group_archive.get("sessions", [])]
        == ["fixture-archive-group"]
        and group_archive["sessions"][0].get("projectID") == "group-research",
        str((status, group_archive)),
    )
    status, empty_archive = mobile_request(
        port, "/mobile/archive?project_id=proj-empty", token
    )
    unknown_status, _ = mobile_request(
        port, "/mobile/archive?project_id=not-a-project", token
    )
    missing_status, _ = mobile_request(port, "/mobile/archive", token)
    case.check(
        "archive distinguishes known-empty, unknown, and missing projects",
        status == 200
        and empty_archive == {"projectID": "proj-empty", "sessions": []}
        and unknown_status == 404
        and missing_status == 400,
        str((status, empty_archive, unknown_status, missing_status)),
    )

    status, _ = mobile_request(port, "/mobile/bootstrap", "wrong-token")
    case.check("an unpaired token is rejected", status == 401, str(status))

    status, output = mobile_request(port, "/mobile/output?session_id=fixture-1&limit=1000", token)
    case.check(
        "output streams from disk with an offset",
        status == 200
        and b"hello from the fixture" in base64.b64decode(output.get("dataBase64", ""))
        and output.get("nextOffset", 0) > 0,
        str(status),
    )

    # These go through unpeel-core::controller_api, not the old TUI route
    # copies. Raw terminal bytes are preserved exactly and ordered; resize
    # uses the shipped phone limits and leaves the phone owning the grid.
    raw_input = "\x1b[A\rhé\x01"
    status, _ = mobile_request(
        port, "/mobile/write", token, method="POST",
        body={"sessionID": "fixture-live", "data": raw_input},
    )
    status2, _ = mobile_request(
        port, "/mobile/write", token, method="POST",
        body={"sessionID": "fixture-live", "data": "second"},
    )
    writes_arrived = driver.wait_for(lambda: len(live_host.writes) >= 2, timeout=5)
    case.check(
        "shared write preserves raw bytes and request order exactly once",
        status == 200
        and status2 == 200
        and bool(writes_arrived)
        and live_host.writes[-2:] == [raw_input, "second"],
        repr(live_host.writes),
    )

    replay_start = len(live_host.writes)
    replay_headers = {"X-Unpeel-Request-ID": "mobile-replay-1"}
    replay_body = {"sessionID": "fixture-live", "data": "only once"}
    replay_status, _ = mobile_request(
        port, "/mobile/write", token, method="POST", body=replay_body,
        headers=replay_headers,
    )
    replay_status_2, _ = mobile_request(
        port, "/mobile/write", token, method="POST", body=replay_body,
        headers=replay_headers,
    )
    replay_arrived = driver.wait_for(
        lambda: len(live_host.writes) >= replay_start + 1, timeout=5
    )
    time.sleep(0.2)
    case.check(
        "stable request IDs suppress duplicate terminal writes",
        replay_status == 200
        and replay_status_2 == 200
        and bool(replay_arrived)
        and live_host.writes[replay_start:] == ["only once"],
        repr(live_host.writes[replay_start:]),
    )

    status, _ = mobile_request(
        port, "/mobile/resize", token, method="POST",
        body={"sessionID": " fixture-live ", "columns": 999, "rows": 1},
    )
    resize_arrived = driver.wait_for(
        lambda: live_host.resizes and live_host.resizes[-1] == (300, 2), timeout=5
    )
    driver.read_for(1.5)
    case.check(
        "shared resize clamps the grid and retains phone ownership",
        status == 200
        and bool(resize_arrived)
        and live_host.resizes[-1] == (300, 2),
        repr(live_host.resizes),
    )

    screenshot_start = len(live_host.writes)
    status, screenshot = mobile_request(
        port, "/mobile/request-screenshot", token, method="POST",
        body={"sessionID": "fixture-live"},
    )
    screenshot_arrived = driver.wait_for(
        lambda: len(live_host.writes) >= screenshot_start + 3, timeout=5
    )
    screenshot_writes = live_host.writes[screenshot_start:screenshot_start + 3]
    case.check(
        "shared screenshot action submits one provider-neutral prompt",
        status == 200
        and screenshot.get("accepted") is True
        and bool(screenshot_arrived)
        and len(screenshot_writes) == 3
        and screenshot_writes[0].startswith("\x1b[200~")
        and "Unpeel Browser tool" in screenshot_writes[0]
        and screenshot_writes[0].endswith("\x1b[201~")
        and screenshot_writes[1:] == ["\r", "\r"],
        repr(screenshot_writes),
    )

    status, _ = mobile_request(
        port, "/mobile/mark-read", token, method="POST",
        body={"sessionID": "fixture-live"},
    )
    receipt = driver.wait_for(
        lambda: home.read_marker("fixture-live", "read.json"), timeout=5
    )
    case.check(
        "shared mark-read writes the cross-frontend receipt",
        status == 200 and isinstance(receipt.get("read_at"), int),
        repr(receipt),
    )

    started = time.monotonic()
    status, _ = mobile_request(
        port, "/mobile/write", token, method="POST",
        body={"sessionID": "fixture-slow", "data": "one uncertain write"},
        timeout=5,
    )
    elapsed = time.monotonic() - started
    case.check(
        "an unresponsive Host is bounded and the write is never replayed",
        status == 500
        and elapsed < 2.8
        and slow_host.writes == ["one uncertain write"],
        f"status={status} elapsed={elapsed:.2f} writes={slow_host.writes!r}",
    )

    status, artifacts = mobile_request(
        port, "/mobile/artifacts?session_id=fixture-1", token
    )
    listed = artifacts.get("artifacts", [])
    case.check(
        "headless gallery lists session artifacts",
        status == 200
        and any(item.get("kind") == "screenshots"
                and item.get("name") == "result.png"
                and item.get("size") == len(screenshot_bytes)
                for item in listed),
        str(artifacts),
    )
    status, first_chunk = mobile_request(
        port,
        "/mobile/artifact?session_id=fixture-1&kind=screenshots&name=result.png&limit=7",
        token,
    )
    status2, second_chunk = mobile_request(
        port,
        "/mobile/artifact?session_id=fixture-1&kind=screenshots&name=result.png&offset=7",
        token,
    )
    streamed = (
        base64.b64decode(first_chunk.get("dataBase64", ""))
        + base64.b64decode(second_chunk.get("dataBase64", ""))
    )
    case.check(
        "headless gallery streams bounded artifact chunks",
        status == 200
        and status2 == 200
        and first_chunk.get("nextOffset") == 7
        and second_chunk.get("contentType") == "image/png"
        and streamed == screenshot_bytes,
        str((status, status2, first_chunk, second_chunk)),
    )
    status, deleted = mobile_request(
        port,
        "/mobile/artifact-delete?session_id=fixture-1&kind=screenshots&name=result.png",
        token,
        method="POST",
    )
    case.check(
        "shared artifact delete removes the Host file idempotently",
        status == 200
        and deleted.get("ok") == "true"
        and not os.path.exists(os.path.join(screenshots, "result.png")),
        str((status, deleted)),
    )

    # Uploads have their own durable idempotency key, independent of a Link
    # request id. Exercise a full-sized first chunk, response-loss replay,
    # offset conflict, publication, gallery readback, and deletion through the
    # same app-less Host routes the phone uses.
    upload_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    upload_bytes = b"\x89PNG\r\n\x1a\n" + b"u" * (262_144 - 8) + b"final-chunk"
    upload_sha256 = hashlib.sha256(upload_bytes).hexdigest()

    def upload_chunk(offset, chunk):
        query = urllib.parse.urlencode({
            "session_id": "fixture-1",
            "upload_id": upload_id,
            "offset": offset,
            "total_size": len(upload_bytes),
            "sha256": upload_sha256,
        })
        return mobile_binary_request(
            port,
            f"/mobile/upload-chunk?{query}",
            token,
            chunk,
            "image/png",
        )

    first = upload_bytes[:262_144]
    final = upload_bytes[262_144:]
    first_status, first_receipt = upload_chunk(0, first)
    retry_status, retry_receipt = upload_chunk(0, first)
    gap_status, gap_receipt = upload_chunk(len(first) + 1, final[:-1])
    final_status, final_receipt = upload_chunk(len(first), final)
    complete_retry_status, complete_retry = upload_chunk(len(first), final)
    uploaded_path = final_receipt.get("path", "")
    case.check(
        "resumable upload accepts a 256 KiB chunk and survives response-loss retry",
        first_status == 200
        and first_receipt.get("complete") is False
        and first_receipt.get("nextOffset") == len(first)
        and retry_status == 200
        and retry_receipt.get("nextOffset") == len(first)
        and gap_status == 409
        and gap_receipt.get("nextOffset") == len(first),
        str((first_status, first_receipt, retry_status, retry_receipt,
             gap_status, gap_receipt)),
    )
    case.check(
        "resumable upload atomically publishes exactly one Host-owned file",
        final_status == 200
        and final_receipt.get("complete") is True
        and final_receipt.get("kind") == "uploads"
        and final_receipt.get("contentType") == "image/png"
        and final_receipt.get("sha256") == upload_sha256
        and complete_retry_status == 200
        and complete_retry.get("path") == uploaded_path
        and uploaded_path
        and os.path.isfile(uploaded_path)
        and open(uploaded_path, "rb").read() == upload_bytes,
        str((final_status, final_receipt, complete_retry_status, complete_retry)),
    )

    status, uploaded_gallery = mobile_request(
        port, "/mobile/artifacts?session_id=fixture-1", token
    )
    upload_name = final_receipt.get("name", "")
    status1, upload_read1 = mobile_request(
        port,
        "/mobile/artifact?" + urllib.parse.urlencode({
            "session_id": "fixture-1",
            "kind": "uploads",
            "name": upload_name,
            "limit": 200_000,
        }),
        token,
    )
    status2, upload_read2 = mobile_request(
        port,
        "/mobile/artifact?" + urllib.parse.urlencode({
            "session_id": "fixture-1",
            "kind": "uploads",
            "name": upload_name,
            "offset": upload_read1.get("nextOffset", 0),
            "limit": 200_000,
        }),
        token,
    )
    uploaded_round_trip = (
        base64.b64decode(upload_read1.get("dataBase64", ""))
        + base64.b64decode(upload_read2.get("dataBase64", ""))
    )
    case.check(
        "uploaded bytes round-trip through gallery list and ranged reads",
        status == 200
        and sum(item.get("kind") == "uploads" and item.get("name") == upload_name
                for item in uploaded_gallery.get("artifacts", [])) == 1
        and status1 == 200
        and status2 == 200
        and upload_read1.get("contentType") == "image/png"
        and upload_read2.get("nextOffset") == len(upload_bytes)
        and upload_read2.get("totalSize") == len(upload_bytes)
        and uploaded_round_trip == upload_bytes,
        str((status, uploaded_gallery, status1, status2)),
    )
    delete_status, delete_receipt = mobile_request(
        port,
        "/mobile/artifact-delete?" + urllib.parse.urlencode({
            "session_id": "fixture-1",
            "kind": "uploads",
            "name": upload_name,
        }),
        token,
        method="POST",
    )
    case.check(
        "uploaded gallery files use the shared idempotent delete path",
        delete_status == 200
        and delete_receipt.get("ok") == "true"
        and not os.path.exists(uploaded_path),
        str((delete_status, delete_receipt)),
    )

    status, _ = mobile_request(
        port, "/mobile/request-screenshot", token, method="POST",
        body={"sessionID": "fixture-1"}
    )
    case.check(
        "typed screenshot requests reject an unavailable session host",
        status == 404,
        str(status),
    )

    status, _ = mobile_request(port, "/mobile/input", token, method="POST",
                               body={"sessionID": "fixture-1", "text": "hi"})
    case.check("writing to a dead host is refused", status == 404, str(status))

    unknown_restart_status, _ = mobile_request(
        port, "/mobile/restart-session", token, method="POST",
        body={"sessionID": "missing-lifecycle-session"},
    )
    unknown_action_status, _ = mobile_request(
        port, "/mobile/session-action", token, method="POST",
        body={"sessionID": "missing-lifecycle-session", "action": "remove"},
    )
    exited_stop_status, _ = mobile_request(
        port, "/mobile/session-action", token, method="POST",
        body={"sessionID": "fixture-1", "action": "stop"},
    )
    old_host_resume_agent_status, _ = mobile_request(
        port, "/mobile/session-action", token, method="POST",
        body={"sessionID": "fixture-old-host", "action": "resume_agent"},
    )
    active_resume_agent_status, _ = mobile_request(
        port, "/mobile/session-action", token, method="POST",
        body={"sessionID": "fixture-active", "action": "resume_agent"},
    )
    live_resume_status, _ = mobile_request(
        port, "/mobile/restart-session", token, method="POST",
        body={"sessionID": "fixture-live"},
    )
    live_action_resume_status, _ = mobile_request(
        port, "/mobile/session-action", token, method="POST",
        body={"sessionID": "fixture-live", "action": "restart"},
    )
    case.check(
        "lifecycle errors fail closed without replacing a live terminal",
        unknown_restart_status == 404
        and unknown_action_status == 404
        and exited_stop_status == 409
        and old_host_resume_agent_status == 409
        and active_resume_agent_status == 409
        and live_resume_status == 409
        and live_action_resume_status == 409,
        str((unknown_restart_status, unknown_action_status, exited_stop_status,
             old_host_resume_agent_status, active_resume_agent_status, live_resume_status,
             live_action_resume_status)),
    )

    agent_resume_headers = {"X-Unpeel-Request-ID": "mobile-agent-resume-once"}
    agent_resume_status, agent_resume_receipt = mobile_request(
        port, "/mobile/session-action", token, method="POST", timeout=15,
        body={"sessionID": "fixture-live", "action": "resume_agent"},
        headers=agent_resume_headers,
    )
    agent_resume_replay_status, agent_resume_replay = mobile_request(
        port, "/mobile/session-action", token, method="POST", timeout=15,
        body={"sessionID": "fixture-live", "action": "resume_agent"},
        headers=agent_resume_headers,
    )
    case.check(
        "Resume Agent is one replay-safe effect and keeps the terminal identity",
        agent_resume_status == 200
        and agent_resume_receipt == {"ok": True}
        and agent_resume_replay_status == 200
        and agent_resume_replay == agent_resume_receipt
        and live_host.resume_agent_generations == [0]
        and home.manifests().get("fixture-live", {}).get("state") == "running",
        str((agent_resume_status, agent_resume_receipt,
             agent_resume_replay_status, agent_resume_replay,
             live_host.resume_agent_generations,
             home.manifests().get("fixture-live"))),
    )

    stop_status, stop_receipt = mobile_request(
        port, "/mobile/session-action", token, method="POST", timeout=15,
        body={"sessionID": created_id, "action": "stop"},
        headers={"X-Unpeel-Request-ID": "mobile-stop-once"},
    )
    created_stopped = driver.wait_for(
        lambda: home.manifests().get(created_id, {}).get("state") == "exited",
        timeout=8,
    )
    case.check(
        "the phone stops a live hosted PTY",
        stop_status == 200
        and stop_receipt == {"ok": True}
        and bool(created_stopped),
        str((stop_status, stop_receipt, home.manifests().get(created_id))),
    )

    lifecycle_title = "Remote lifecycle title"
    home.marker(created_id, "title.json", {"title": lifecycle_title, "updated_at": 1})
    home.pin(created_id, project_id="proj-1", pinned_at=777)
    lifecycle_state = home.state()
    for entries in lifecycle_state.get("pinned_sessions", {}).values():
        for entry in entries:
            if entry.get("session_id") == created_id:
                entry["future_pin_field"] = {"kept": True}
    lifecycle_state["mcp_orchestrators"] = {
        created_id: {"role": "write", "future": {"kept": True}},
        "fixture-2": {"role": "read"},
    }
    lifecycle_state["mcp_write_approvals"] = {
        created_id: ["fixture-2", created_id],
        "fixture-2": [created_id],
    }
    lifecycle_state["browser_approvals"] = ["fixture-2", created_id]
    lifecycle_state["computer_approvals"] = [created_id, "fixture-2"]
    home.write_state(lifecycle_state)
    with open(home.path("session-order.json"), "w") as handle:
        json.dump({"proj-1": ["fixture-1", created_id, "fixture-2"]}, handle)

    # The created session's fake agent leaves no conversation data; give it
    # the resume evidence the replacement-restart gate requires.
    home.seed_resume_data(created_id)
    before_restart = set(home.manifests())
    restart_headers = {"X-Unpeel-Request-ID": "mobile-restart-once"}
    restart_status, restart_receipt = mobile_request(
        port, "/mobile/restart-session", token, method="POST", timeout=20,
        body={"sessionID": created_id}, headers=restart_headers,
    )
    restart_replay_status, restart_replay = mobile_request(
        port, "/mobile/restart-session", token, method="POST", timeout=20,
        body={"sessionID": created_id}, headers=restart_headers,
    )
    replacement = {}

    def replacement_is_live():
        nonlocal replacement
        manifests = home.manifests()
        replacement_ids = set(manifests).difference(before_restart)
        if len(replacement_ids) != 1:
            return False
        replacement_id = next(iter(replacement_ids))
        replacement = manifests.get(replacement_id, {})
        return (
            created_id not in manifests
            and replacement.get("state") == "running"
        )

    replacement_live = driver.wait_for(replacement_is_live, timeout=12)
    replacement_id = replacement.get("session", {}).get("id")
    restarted_state = home.state()
    restarted_pins = [
        entry
        for entries in restarted_state.get("pinned_sessions", {}).values()
        for entry in entries
        if entry.get("session_id") == replacement_id
    ]
    try:
        with open(home.path("session-order.json")) as handle:
            restarted_order = json.load(handle)
    except (OSError, ValueError):
        restarted_order = {}
    orchestrators = restarted_state.get("mcp_orchestrators", {})
    write_approvals = restarted_state.get("mcp_write_approvals", {})
    case.check(
        "restart is one effect and transfers the session identity",
        restart_status == 200
        and restart_receipt == {"ok": True}
        and restart_replay_status == 200
        and restart_replay == restart_receipt
        and bool(replacement_live)
        and bool(replacement_id)
        and replacement.get("session", {}).get("label") == lifecycle_title
        and replacement.get("session", {}).get("custom_title") is True
        and len(restarted_pins) == 1
        and restarted_pins[0].get("key") == f"session:{replacement_id}"
        and restarted_pins[0].get("pinned_at") == 777
        and restarted_pins[0].get("future_pin_field") == {"kept": True}
        and created_id not in orchestrators
        and orchestrators.get(replacement_id, {}).get("future") == {"kept": True}
        and created_id not in write_approvals
        and write_approvals.get(replacement_id) == ["fixture-2", replacement_id]
        and write_approvals.get("fixture-2") == [replacement_id]
        and restarted_state.get("browser_approvals") == ["fixture-2", replacement_id]
        and restarted_state.get("computer_approvals") == [replacement_id, "fixture-2"]
        and restarted_order.get("proj-1")
        == ["fixture-1", replacement_id, "fixture-2"],
        str((restart_status, restart_receipt, restart_replay_status,
             restart_replay, replacement, restarted_pins, orchestrators,
             write_approvals, restarted_order)),
    )

    remove_headers = {"X-Unpeel-Request-ID": "mobile-remove-once"}
    remove_status, remove_receipt = mobile_request(
        port, "/mobile/session-action", token, method="POST", timeout=20,
        body={"sessionID": replacement_id, "action": "remove"},
        headers=remove_headers,
    )
    remove_replay_status, remove_replay = mobile_request(
        port, "/mobile/session-action", token, method="POST", timeout=20,
        body={"sessionID": replacement_id, "action": "remove"},
        headers=remove_headers,
    )
    removed_state = home.state()
    removed_pin_ids = [
        entry.get("session_id")
        for entries in removed_state.get("pinned_sessions", {}).values()
        for entry in entries
    ]
    try:
        with open(home.path("session-order.json")) as handle:
            removed_order = json.load(handle)
    except (OSError, ValueError):
        removed_order = {}
    case.check(
        "remove deletes once, prunes identity, and replays its receipt",
        remove_status == 200
        and remove_receipt == {"ok": True}
        and remove_replay_status == 200
        and remove_replay == remove_receipt
        and replacement_id
        and not os.path.exists(home.path("app-sessions", replacement_id))
        and replacement_id not in removed_pin_ids
        and replacement_id not in removed_state.get("mcp_orchestrators", {})
        and replacement_id not in removed_state.get("mcp_write_approvals", {})
        and "fixture-2" not in removed_state.get("mcp_write_approvals", {})
        and removed_state.get("browser_approvals") == ["fixture-2"]
        and removed_state.get("computer_approvals") == ["fixture-2"]
        and removed_order.get("proj-1") == ["fixture-1", "fixture-2"],
        str((remove_status, remove_receipt, remove_replay_status, remove_replay,
             replacement_id, removed_state, removed_order)),
    )

    status, _ = mobile_request(port, "/mobile/session-organization", token, method="POST",
                               body={"sessionID": "fixture-1", "archived": True})
    case.check(
        "the phone can archive",
        status == 200 and home.has_marker("fixture-1", "archived.json"),
        str(status),
    )
    archive_after = {}

    def archived_snapshot_contains_fixture():
        nonlocal archive_after
        archive_status, archive_after = mobile_request(
            port, "/mobile/archive?project_id=proj-1", token
        )
        return archive_status == 200 and any(
            item.get("id") == "fixture-1"
            for item in archive_after.get("sessions", [])
        )

    published = driver.wait_for(archived_snapshot_contains_fixture, timeout=12)
    case.check(
        "archive writes publish into the live Controller catalog",
        bool(published),
        str(archive_after),
    )
    status, _ = mobile_request(port, "/mobile/session-organization", token, method="POST",
                               body={"sessionID": "fixture-1", "archived": False})
    case.check(
        "the phone can restore",
        status == 200 and not home.has_marker("fixture-1", "archived.json"),
        str(status),
    )
    archive_after_restore = {}

    def archived_snapshot_drops_fixture():
        nonlocal archive_after_restore
        archive_status, archive_after_restore = mobile_request(
            port, "/mobile/archive?project_id=proj-1", token
        )
        return archive_status == 200 and all(
            item.get("id") != "fixture-1"
            for item in archive_after_restore.get("sessions", [])
        )

    restored_published = driver.wait_for(archived_snapshot_drops_fixture, timeout=12)
    case.check(
        "restore writes disappear from the live Controller catalog",
        bool(restored_published),
        str(archive_after_restore),
    )

    status, _ = mobile_request(port, "/mobile/not-a-route", token)
    case.check("unknown routes 404", status == 404, str(status))

    # ── polite guest: server-port is the app's rebinding contract ──
    with open(home.path("mobile", "server-port")) as handle:
        after = handle.read().strip()
    case.check(
        "the Host owner never rewrites the app's port file",
        after == str(port),
        f"expected {port}, found {after!r} — rewriting it orphans the phone",
    )
    driver.close()
    case.check(
        "serve exits cleanly",
        driver.exited() and not os.path.exists(home.path("serve.json")),
        driver.log(),
    )
    with open(home.path("mobile", "server-port")) as handle:
        case.check(
            "and leaves it intact on exit",
            handle.read().strip() == str(port),
        )


if __name__ == "__main__":
    run("mobile", body)
