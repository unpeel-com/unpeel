//
//  Snapshot.swift
//  UnpeelNative
//
//  Self-snapshot verification loop ("the agent's eyes"):
//    UNPEEL_SNAPSHOT=/path.png   render window.contentView to PNG after 3s
//    UNPEEL_SNAPSHOT_QUIT=1      terminate after writing
//    UNPEEL_SNAPSHOT_DELAY=<s>   override the 3s delay
//    UNPEEL_SELECT_SESSION=<id>  auto-select a session before the snapshot
//    UNPEEL_TEST_LAUNCH=<project-id>|<command>
//                                 exercise the real in-app launch path
//                                 (launch file -> unpeel-host -> manifest
//                                 poll -> select + attach)
//    UNPEEL_EXPAND_PROJECTS=<id,id,...>
//                                 expand projects (animated) at t=0.5s
//    UNPEEL_SHOW_ALL_SESSIONS=<id,id,...>
//                                 trigger "Show N more" (reveal all session
//                                 rows) for those projects at t=1.0s
//    UNPEEL_SCROLL_SIDEBAR=<px>  scroll the sidebar list down <px> points
//                                 at t=1.4s (clamped; verifies rows melting
//                                 into the top progressive blur)
//    UNPEEL_DISABLE_TOP_BLUR=1   omit the sidebar's progressive top blur
//                                 (perf A/B baseline; lives in SidebarView)
//    UNPEEL_OPEN_WORKTREES=<project-id>
//                                 expand the project so its inline worktree
//                                 folder rows show, at t=0.5s
//    UNPEEL_COLLAPSE_SIDEBAR=1   collapse the sidebar (animated) at t=0.5s
//    UNPEEL_OPEN_SETTINGS=1|presets|advanced
//                                 open settings at t=0.5s (sidebar nav
//                                 slides in, content pane swaps to panel)
//    UNPEEL_CLOSE_SETTINGS=<s>   close settings again at t=<s> (verify
//                                 the workspace/terminal after a round trip)
//
//  Note: NSVisualEffectView blur does not render through cacheDisplay —
//  vibrancy regions come out flat/dark. Judge layout/colors/typography.
//  Metal-backed terminal surfaces may also render empty in cacheDisplay.
//  macOS 26 Liquid Glass controls (.glass/.glassProminent buttons, switch
//  thumbs) are worse: their backdrop layer floods the pane WHITE in
//  cacheDisplay. Use UNPEEL_SNAPSHOT_WINDOW=1 (composited capture) for
//  any view containing native glass controls, e.g. the settings panels.
//

import AppKit
import SwiftUI

@MainActor
enum Snapshot {
    static func armIfRequested(
        window: NSWindow, store: UnpeelStore, cache: SurfaceCache? = nil
    ) {
        let env = ProcessInfo.processInfo.environment
        guard let path = env["UNPEEL_SNAPSHOT"], !path.isEmpty else { return }

        // UNPEEL_DUMP_SCREEN=/path.txt: write the selected session's
        // terminal screen text (ghostty_surface_read_text) alongside the
        // snapshot. Metal surfaces don't render through cacheDisplay, so
        // this is how verification proves the visible pane actually drew.
        if let dumpPath = env["UNPEEL_DUMP_SCREEN"], !dumpPath.isEmpty, let cache {
            let delay = env["UNPEEL_SNAPSHOT_DELAY"].flatMap(Double.init) ?? 3
            DispatchQueue.main.asyncAfter(deadline: .now() + delay - 0.2) {
                let text = store.selectedSessionID
                    .flatMap { cache.existingPane(for: $0) }
                    .flatMap { $0.dumpScreenText() } ?? "<no pane>"
                try? text.write(
                    toFile: dumpPath, atomically: true, encoding: .utf8
                )
            }
        }

        if let sessionID = env["UNPEEL_SELECT_SESSION"], !sessionID.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                store.rescan()
                store.selectedSessionID = sessionID
            }
        }

        // UNPEEL_SELECT_SESSIONS=<id,id,...>: select each session in turn
        // (staggered) so the surface cache attaches and retains a pane per
        // session, ending on the last id. Used by CPU/perf verification to
        // exercise hidden retained surfaces.
        if let ids = env["UNPEEL_SELECT_SESSIONS"], !ids.isEmpty {
            let sessionIDs = ids.split(separator: ",").map(String.init)
            for (index, sessionID) in sessionIDs.enumerated() {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6 + Double(index) * 0.5) {
                    if index == 0 { store.rescan() }
                    store.selectedSessionID = sessionID
                }
            }
        }

        // UNPEEL_TEST_PROJECT_PATH=<dir>: register (and expand) an
        // ephemeral in-memory project for that directory so existing
        // sessions launched there in a previous run render in the sidebar.
        if let path = env["UNPEEL_TEST_PROJECT_PATH"], !path.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                let id = store.addEphemeralProject(path: path)
                store.expandedProjectIDs.insert(id)
            }
        }

        if let spec = env["UNPEEL_TEST_LAUNCH"], !spec.isEmpty {
            let parts = spec.split(separator: "|", maxSplits: 1).map(String.init)
            if let target = parts.first {
                let command = parts.count > 1 ? parts[1] : ""
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                    // `path:/some/dir|command` launches inside an ephemeral
                    // in-memory project for that directory (hook
                    // verification in temp dirs without touching any
                    // persisted project list); otherwise the spec is a
                    // project id.
                    let projectID = target.hasPrefix("path:")
                        ? store.addEphemeralProject(path: String(target.dropFirst("path:".count)))
                        : target
                    store.launchSession(projectID: projectID, command: command)
                }
            }
        }

        if let ids = env["UNPEEL_EXPAND_PROJECTS"], !ids.isEmpty {
            let projectIDs = ids.split(separator: ",").map(String.init)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                withAnimation(SidebarMotion.accordionOpen) {
                    store.expandedProjectIDs.formUnion(projectIDs)
                }
            }
        }

        // ("Show N more" is gone — the sidebar always shows every active
        // session plus the inactive preview, so the old
        // UNPEEL_SHOW_ALL_SESSIONS reveal hook has nothing left to reveal.)

        // UNPEEL_SCROLL_SIDEBAR=<px>: scroll the sidebar's scroll view
        // down by <px> points (clamped to the document height) after the
        // expand/launch hooks settle. Drives the leftmost NSScrollView
        // directly — SwiftUI scrollTo on a LazyVStack overshoots when row
        // heights are still estimated.
        if let raw = env["UNPEEL_SCROLL_SIDEBAR"], let px = Double(raw) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.4) {
                func scrollViews(in view: NSView) -> [NSScrollView] {
                    var found: [NSScrollView] = []
                    if let scroll = view as? NSScrollView { found.append(scroll) }
                    for sub in view.subviews { found.append(contentsOf: scrollViews(in: sub)) }
                    return found
                }
                guard let content = window.contentView else { return }
                let sidebarScroll = scrollViews(in: content).min { a, b in
                    a.convert(a.bounds, to: nil).minX < b.convert(b.bounds, to: nil).minX
                }
                guard let scroll = sidebarScroll, let doc = scroll.documentView else { return }
                let maxOffset = max(0, doc.bounds.height - scroll.contentView.bounds.height)
                let target = min(CGFloat(px), maxOffset)
                let y = doc.isFlipped ? target : maxOffset - target
                scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
                scroll.reflectScrolledClipView(scroll.contentView)
            }
        }

        // UNPEEL_SCROLL_SETTINGS=<px>: scroll the settings content pane's
        // scroll view (the RIGHTMOST one — the sidebar owns the leftmost)
        // down by <px> points at t=1.6s. Verifies the settings panels
        // scroll internally instead of growing the window.
        if let raw = env["UNPEEL_SCROLL_SETTINGS"], let px = Double(raw) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.6) {
                func scrollViews(in view: NSView) -> [NSScrollView] {
                    var found: [NSScrollView] = []
                    if let scroll = view as? NSScrollView { found.append(scroll) }
                    for sub in view.subviews { found.append(contentsOf: scrollViews(in: sub)) }
                    return found
                }
                guard let content = window.contentView else { return }
                let contentScroll = scrollViews(in: content).max { a, b in
                    a.convert(a.bounds, to: nil).minX < b.convert(b.bounds, to: nil).minX
                }
                guard let scroll = contentScroll, let doc = scroll.documentView else { return }
                let maxOffset = max(0, doc.bounds.height - scroll.contentView.bounds.height)
                let target = min(CGFloat(px), maxOffset)
                let y = doc.isFlipped ? target : maxOffset - target
                scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
                scroll.reflectScrolledClipView(scroll.contentView)
                NSLog(
                    "[UnpeelNative] settings scroll: doc=%.0f viewport=%.0f offset=%.0f",
                    doc.bounds.height, scroll.contentView.bounds.height, target
                )
            }
        }

        // UNPEEL_OPEN_ARCHIVE=<project-id> — open that project's archived
        // sessions page at t=0.5s (content-pane swap, same as the project
        // context menu's "Archived sessions").
        if let projectID = env["UNPEEL_OPEN_ARCHIVE"], !projectID.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                store.openArchivedSessions(projectID: projectID)
            }
        }

        // Worktrees render inline under the parent now (no slide-in panel):
        // expand the parent so its worktree folder rows are photographable.
        if let projectID = env["UNPEEL_OPEN_WORKTREES"], !projectID.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                withAnimation(SidebarMotion.accordionOpen) {
                    _ = store.expandedProjectIDs.insert(projectID)
                }
            }
        }

        // "1" collapses, "0" expands. NOTE: the store persists this flag on
        // change, so a verification run that overrides it should restore the
        // original `unpeel.sidebar.collapsed` default afterwards.
        if let raw = env["UNPEEL_COLLAPSE_SIDEBAR"], raw == "1" || raw == "0" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                store.sidebarCollapsed = raw == "1"
            }
        }

        // UNPEEL_OPEN_SETTINGS=1|presets|advanced — open settings on the
        // given tab (default presets): the sidebar slides to the settings
        // nav and the content pane swaps to the panel.
        if let raw = env["UNPEEL_OPEN_SETTINGS"], !raw.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                // "license" was the standalone Unpeel Link tab until it merged
                // into Remote (2026-08-13) — keep old deep links working.
                let fallback: SettingsTab = raw == "license" ? .mobile : .presets
                store.openSettings(tab: SettingsTab.compatibleRawValue(raw) ?? fallback)
            }
        }

        // UNPEEL_CLOSE_SETTINGS=<seconds> — close settings at t=<s>, so a
        // snapshot can verify the open→close round trip leaves the
        // workspace (and the retained terminal surface) intact.
        if let raw = env["UNPEEL_CLOSE_SETTINGS"], let at = Double(raw) {
            DispatchQueue.main.asyncAfter(deadline: .now() + at) {
                store.closeSettings()
            }
        }

        // UNPEEL_DEBUG_RENAME_SESSION=<session-id> — put that session's
        // row into the inline rename-editor state at t=1.2s (after
        // expansion settles) so a snapshot can photograph it. UI state
        // only; nothing is committed.
        if let sessionID = env["UNPEEL_DEBUG_RENAME_SESSION"], !sessionID.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                store.editingSessionID = sessionID
            }
        }

        // UNPEEL_DEBUG_CONFIRM_REMOVE=<session-id>[@archive] — put that
        // session's row into the inline "Remove session?" confirm state at
        // t=1.2s (after expansion settles) so a snapshot can photograph it.
        // The @archive suffix requests the archive-page surface (pair with
        // UNPEEL_OPEN_ARCHIVE).
        if let spec = env["UNPEEL_DEBUG_CONFIRM_REMOVE"], !spec.isEmpty {
            let parts = spec.split(separator: "@", maxSplits: 1).map(String.init)
            let surface: UnpeelStore.RemoveConfirmSurface =
                parts.count > 1 && parts[1] == "archive" ? .archivePage : .sidebar
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                store.requestRemoveSession(parts[0], from: surface)
            }
        }

        // UNPEEL_TEST_REMOVE=<session-id> — run the full remove flow
        // (confirm → socket kill → SIGKILL fallback → dir removal → prune)
        // at t=1.2s; the caller asserts the session dir is gone afterwards.
        if let sessionID = env["UNPEEL_TEST_REMOVE"], !sessionID.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                store.requestRemoveSession(sessionID)
                store.confirmRemoveSession(sessionID)
            }
        }

        // UNPEEL_DEBUG_CONFIRM_REMOVE_PROJECT=<project-id> — put that
        // project's row into the inline "Remove project?" confirm state at
        // t=1.2s so a snapshot can photograph it. UI state only.
        if let projectID = env["UNPEEL_DEBUG_CONFIRM_REMOVE_PROJECT"], !projectID.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                store.requestRemoveProject(projectID)
            }
        }

        // UNPEEL_TEST_RESTART=<session-id>[@<seconds>] — programmatic
        // "Restart" click (the DeadSessionView button path) at t=<s>
        // (default 1.2s). The caller asserts a fresh running session with
        // the same command/cwd exists afterwards.
        if let spec = env["UNPEEL_TEST_RESTART"], !spec.isEmpty {
            let parts = spec.split(separator: "@", maxSplits: 1).map(String.init)
            let sessionID = parts[0]
            let at = parts.count > 1 ? (Double(parts[1]) ?? 1.2) : 1.2
            DispatchQueue.main.asyncAfter(deadline: .now() + at) {
                store.rescan()
                store.restartSession(sessionID)
            }
        }

        // UNPEEL_TEST_WORKTREE=<parentPath>|<worktreePath>|<branch> —
        // register an ephemeral parent project (worktrees enabled) with an
        // ephemeral worktree CHILD project, so the worktrees link row /
        // spinner can be photographed without touching real projects.
        if let spec = env["UNPEEL_TEST_WORKTREE"], !spec.isEmpty {
            let parts = spec.split(separator: "|").map(String.init)
            if parts.count == 3 {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    let childID = store.addEphemeralWorktreeProject(
                        parentPath: parts[0], path: parts[1], branch: parts[2]
                    )
                    if let child = store.projectsByID[childID],
                       let parentID = child.parentProjectID {
                        store.expandedProjectIDs.insert(parentID)
                    }
                }
            }
        }

        // UNPEEL_DEBUG_OPEN_PRESETS=1 — open Settings on the Presets tab.
        // Older detail/detail-off values still open the same single-screen
        // inline editor; there is no preset drill-in route anymore.
        if let presetDebug = env["UNPEEL_DEBUG_OPEN_PRESETS"], !presetDebug.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                store.openSettings(tab: .presets)
            }
        }

        let delay = env["UNPEEL_SNAPSHOT_DELAY"].flatMap(Double.init) ?? 3
        // Re-activate just before capture: the launching shell can steal
        // focus after the AppDelegate's initial activate, and an inactive
        // window renders desaturated controls (gray traffic lights, gray
        // tinted buttons/switches) — not what the user sees.
        DispatchQueue.main.asyncAfter(deadline: .now() + max(0, delay - 0.4)) {
            NSRunningApplication.current.activate(
                options: [.activateIgnoringOtherApps, .activateAllWindows]
            )
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
        }
        // UNPEEL_SNAPSHOT2=/path.png (+ UNPEEL_SNAPSHOT2_DELAY=<s>, default
        // delay+0.5): a SECOND capture in the same run — verifies animated
        // states (e.g. the busy shimmer mid-sweep at two phases). When set,
        // SNAPSHOT_QUIT terminates after the second capture instead.
        let secondPath = env["UNPEEL_SNAPSHOT2"]
        let secondDelay = env["UNPEEL_SNAPSHOT2_DELAY"].flatMap(Double.init) ?? (delay + 0.5)
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            write(window: window, to: path)
            if env["UNPEEL_SNAPSHOT_QUIT"] == "1", secondPath == nil {
                NSApp.terminate(nil)
            }
        }
        if let secondPath, !secondPath.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + max(secondDelay, delay + 0.1)) {
                write(window: window, to: secondPath)
                if env["UNPEEL_SNAPSHOT_QUIT"] == "1" {
                    NSApp.terminate(nil)
                }
            }
        }
    }

    /// UNPEEL_TEST_REORDER=1 (+ UNPEEL_TEST_PROJECT_PATH=<dir> with ≥2
    /// hosted sessions): headless round-trip test of the drag-reorder
    /// overlays. Verifies newest-first base order, project + session move
    /// semantics, persistence across a fresh store, and stale-id GC; then
    /// restores the overlay keys exactly as found and terminates.
    static func runReorderSelfTestIfRequested(store: UnpeelStore) {
        let env = ProcessInfo.processInfo.environment
        guard env["UNPEEL_TEST_REORDER"] == "1",
              let projectPath = env["UNPEEL_TEST_PROJECT_PATH"], !projectPath.isEmpty
        else { return }

        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            var failures = 0
            func expect(_ condition: Bool, _ message: String) {
                if !condition { failures += 1 }
                NSLog("[reorder-test] %@: %@", condition ? "PASS" : "FAIL", message)
            }

            let defaults = AppDefaults.shared
            let projectID = store.addEphemeralProject(path: projectPath)
            let sessionKey = UnpeelStore.sessionOrderKey(projectID)
            let pinnedKey = UnpeelStore.pinnedOrderKey(projectID)
            let worktreeProjectKey = UnpeelStore.projectOrderKey(forParent: projectID)
            // Snapshot the keys as found so the user's real overlay (if
            // any) is restored byte-for-byte.
            let savedProjectOrder = defaults.stringArray(forKey: UnpeelStore.projectOrderKey)
            let savedWorktreeProjectOrder = defaults.stringArray(forKey: worktreeProjectKey)
            let savedSessionOrder = defaults.stringArray(forKey: sessionKey)
            let savedPinnedOrder = defaults.stringArray(forKey: pinnedKey)
            let savedPins = defaults.data(forKey: "unpeel.sidebar.pins")
            defer {
                if let savedProjectOrder {
                    defaults.set(savedProjectOrder, forKey: UnpeelStore.projectOrderKey)
                } else {
                    defaults.removeObject(forKey: UnpeelStore.projectOrderKey)
                }
                if let savedWorktreeProjectOrder {
                    defaults.set(savedWorktreeProjectOrder, forKey: worktreeProjectKey)
                } else {
                    defaults.removeObject(forKey: worktreeProjectKey)
                }
                if let savedSessionOrder {
                    defaults.set(savedSessionOrder, forKey: sessionKey)
                } else {
                    defaults.removeObject(forKey: sessionKey)
                }
                if let savedPinnedOrder {
                    defaults.set(savedPinnedOrder, forKey: pinnedKey)
                } else {
                    defaults.removeObject(forKey: pinnedKey)
                }
                if let savedPins {
                    defaults.set(savedPins, forKey: "unpeel.sidebar.pins")
                } else {
                    defaults.removeObject(forKey: "unpeel.sidebar.pins")
                }
                store.rescan()
                NSLog(
                    "[reorder-test] DONE: %@",
                    failures == 0 ? "ALL PASS" : "\(failures) FAILURE(S)"
                )
                NSApp.terminate(nil)
            }

            // ── Item 1 baseline: regular sessions are newest-first.
            guard let node = store.nodes.first(where: { $0.id == projectID }),
                  node.sessions.count >= 2
            else {
                expect(false, "test project has >=2 sessions")
                return
            }
            let created = node.sessions.map(\.createdAt)
            expect(
                created == created.sorted(by: >),
                "base session order is newest-first (created_at desc)"
            )

            // ── Project reorder: move the ephemeral project to the front.
            let beforeProjects = store.nodes.map(\.id)
            guard beforeProjects.count >= 2, let firstID = beforeProjects.first
            else {
                expect(false, "at least 2 top-level projects to reorder")
                return
            }
            store.moveProject(draggedID: projectID, over: firstID)
            expect(
                store.nodes.first?.id == projectID,
                "moveProject puts dragged project first"
            )
            let othersBefore = beforeProjects.filter { $0 != projectID }
            let othersAfter = store.nodes.map(\.id).filter { $0 != projectID }
            expect(
                othersBefore == othersAfter,
                "other projects keep their relative order"
            )

            // ── Worktree reorder: worktree child projects sort among their
            // inline siblings under the parent, backed by their own
            // project-order key.
            let worktreePathA = URL(fileURLWithPath: projectPath)
                .appendingPathComponent(".unpeel-test-worktree-a")
                .path
            let worktreePathB = URL(fileURLWithPath: projectPath)
                .appendingPathComponent(".unpeel-test-worktree-b")
                .path
            _ = store.addEphemeralWorktreeProject(
                parentPath: projectPath, path: worktreePathA, branch: "test-worktree-a"
            )
            _ = store.addEphemeralWorktreeProject(
                parentPath: projectPath, path: worktreePathB, branch: "test-worktree-b"
            )
            @MainActor func worktreeIDs(of store: UnpeelStore) -> [String] {
                (store.nodes.first { $0.id == projectID })?.worktrees.map(\.id) ?? []
            }
            let beforeWorktrees = worktreeIDs(of: store)
            guard beforeWorktrees.count >= 2 else {
                expect(false, "test project has >=2 worktrees")
                return
            }
            store.moveProject(draggedID: beforeWorktrees.last!, over: beforeWorktrees.first!)
            let expectedWorktrees = [beforeWorktrees.last!] + beforeWorktrees.dropLast()
            expect(
                worktreeIDs(of: store) == expectedWorktrees,
                "moveProject reorders worktree siblings"
            )

            // ── Session reorder: drag the oldest REGULAR session to the
            // top (pinned rows are excluded from drag, like the view).
            let pinnedIDs = Set(
                (store.pinnedByProject[projectID] ?? []).compactMap(\.sessionID)
            )
            @MainActor func regularIDs(of store: UnpeelStore) -> [String] {
                (store.nodes.first { $0.id == projectID })?
                    .sessions.map(\.id).filter { !pinnedIDs.contains($0) } ?? []
            }
            let before = regularIDs(of: store)
            guard before.count >= 2 else {
                expect(false, "test project has >=2 regular sessions")
                return
            }
            store.moveSession(
                projectID: projectID,
                draggedID: before.last!,
                over: before.first!
            )
            let expected = [before.last!] + before.dropLast()
            expect(
                regularIDs(of: store) == expected,
                "moveSession reorders within the regular section"
            )

            // ── Persistence: a FRESH store (new process equivalent — it
            // re-reads UserDefaults + disk) sees all orders.
            let reloaded = UnpeelStore()
            _ = reloaded.addEphemeralProject(path: projectPath)
            _ = reloaded.addEphemeralWorktreeProject(
                parentPath: projectPath, path: worktreePathA, branch: "test-worktree-a"
            )
            _ = reloaded.addEphemeralWorktreeProject(
                parentPath: projectPath, path: worktreePathB, branch: "test-worktree-b"
            )
            expect(
                reloaded.nodes.first?.id == projectID,
                "project order survives a store reload"
            )
            expect(
                regularIDs(of: reloaded) == expected,
                "session order survives a store reload"
            )
            expect(
                worktreeIDs(of: reloaded) == expectedWorktrees,
                "worktree order survives a store reload"
            )

            // ── GC: a stale id in the overlay is skipped at read (the
            // order is unaffected) and dropped when the next drag
            // persists a fresh order.
            var polluted = defaults.stringArray(forKey: sessionKey) ?? []
            polluted.insert("stale-session-id-gc-test", at: 0)
            defaults.set(polluted, forKey: sessionKey)
            store.rescan()
            expect(
                regularIDs(of: store) == expected,
                "stale overlay id is ignored at read time"
            )
            store.moveSession(
                projectID: projectID,
                draggedID: expected.last!,
                over: expected.first!
            )
            let afterGC = defaults.stringArray(forKey: sessionKey) ?? []
            expect(
                !afterGC.contains("stale-session-id-gc-test"),
                "stale id is GC'd when the next order is persisted"
            )

            // ── Pinned reorder: pin the two oldest sessions, then drag the
            // bottom pin above the top one. The reorder must stay inside the
            // pinned section and leave the regular order untouched.
            let toPin = Array(regularIDs(of: store).suffix(2))
            guard toPin.count == 2 else {
                expect(false, "need >=2 sessions to exercise pinned reorder")
                return
            }
            for id in toPin { store.pinSession(projectID: projectID, sessionID: id) }
            @MainActor func pinnedIDsNow(_ store: UnpeelStore) -> [String] {
                (store.pinnedByProject[projectID] ?? []).compactMap(\.sessionID)
            }
            // Regular section now excludes the freshly pinned ids (recompute
            // against the live pinned set, not the snapshot from above).
            @MainActor func liveRegularIDs(_ store: UnpeelStore) -> [String] {
                let pinned = Set(pinnedIDsNow(store))
                return (store.nodes.first { $0.id == projectID })?
                    .sessions.map(\.id).filter { !pinned.contains($0) } ?? []
            }
            let pinnedBefore = pinnedIDsNow(store)
            let regularBeforePinDrag = liveRegularIDs(store)
            expect(pinnedBefore.count == 2, "both sessions are pinned")
            store.movePinnedSession(
                projectID: projectID,
                draggedID: pinnedBefore.last!,
                over: pinnedBefore.first!
            )
            let pinnedExpected = [pinnedBefore.last!] + pinnedBefore.dropLast()
            expect(
                pinnedIDsNow(store) == pinnedExpected,
                "movePinnedSession reorders within the pinned section"
            )
            expect(
                liveRegularIDs(store) == regularBeforePinDrag,
                "reordering pins leaves the regular section untouched"
            )

            // Pinned order survives a fresh store.
            let reloadedPins = UnpeelStore()
            _ = reloadedPins.addEphemeralProject(path: projectPath)
            expect(
                pinnedIDsNow(reloadedPins) == pinnedExpected,
                "pinned order survives a store reload"
            )
        }
    }

    /// UNPEEL_TEST_RENAME=1: headless round-trip test of the native rename
    /// overlay (unpeel.native.sessionTitles): rename → merged label wins →
    /// fresh-store survival → empty rename rejected → stale-id GC. The
    /// overlay key is snapshotted first and restored byte-for-byte, so the
    /// user's real renames (if any) are untouched; manifests are never
    /// written.
    static func runRenameSelfTestIfRequested(store: UnpeelStore) {
        guard ProcessInfo.processInfo.environment["UNPEEL_TEST_RENAME"] == "1" else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            var failures = 0
            func expect(_ condition: Bool, _ message: String) {
                if !condition { failures += 1 }
                NSLog("[rename-test] %@: %@", condition ? "PASS" : "FAIL", message)
            }

            let defaults = AppDefaults.shared
            let saved = defaults.dictionary(forKey: NativeOverlay.sessionTitlesKey)
            defer {
                if let saved {
                    defaults.set(saved, forKey: NativeOverlay.sessionTitlesKey)
                } else {
                    defaults.removeObject(forKey: NativeOverlay.sessionTitlesKey)
                }
                store.rescan()
                NSLog(
                    "[rename-test] DONE: %@",
                    failures == 0 ? "ALL PASS" : "\(failures) FAILURE(S)"
                )
                NSApp.terminate(nil)
            }

            // Deterministic target: lowest session id.
            guard let session = store.sessionsByID.values.min(by: { $0.id < $1.id }) else {
                expect(false, "at least one session on disk to rename")
                return
            }
            let originalLabel = session.label
            let testTitle = "rename-overlay-selftest"

            store.renameSession(session.id, to: "  \(testTitle)  ")
            expect(
                store.sessionsByID[session.id]?.label == testTitle,
                "renamed label (trimmed) wins over the manifest label"
            )

            // Survives a fresh store (new process equivalent).
            let reloaded = UnpeelStore()
            expect(
                reloaded.sessionsByID[session.id]?.label == testTitle,
                "rename survives a store reload"
            )

            // Empty input never reaches the overlay (the view reverts; the
            // store also guards).
            store.renameSession(session.id, to: "   ")
            expect(
                store.sessionsByID[session.id]?.label == testTitle,
                "blank rename is rejected"
            )

            // GC: an entry whose session dir vanished is dropped on the
            // next scan.
            var polluted = (defaults.dictionary(forKey: NativeOverlay.sessionTitlesKey)
                as? [String: String]) ?? [:]
            polluted["stale-rename-gc-test"] = "gone"
            defaults.set(polluted, forKey: NativeOverlay.sessionTitlesKey)
            store.rescan()
            let afterGC = (defaults.dictionary(forKey: NativeOverlay.sessionTitlesKey)
                as? [String: String]) ?? [:]
            expect(
                afterGC["stale-rename-gc-test"] == nil,
                "overlay entry for a vanished session dir is GC'd"
            )
            expect(
                afterGC[session.id] == testTitle,
                "live session's rename survives the GC sweep"
            )
            _ = originalLabel // restored via the saved-overlay defer
        }
    }

    /// UNPEEL_TEST_PRESETS=1: headless round-trip test of the native
    /// preset overlay — global-presets-only model — (add → quick-launch
    /// grouping → reload survival → remove), then wipes the overlay key
    /// so no residue is left in UserDefaults, and terminates. Never touches
    /// app-state.json — every mutation lives only in the overlay key.
    static func runPresetSelfTestIfRequested(store: UnpeelStore) {
        guard ProcessInfo.processInfo.environment["UNPEEL_TEST_PRESETS"] == "1" else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
            var failures = 0
            func expect(_ condition: Bool, _ message: String) {
                if !condition { failures += 1 }
                NSLog("[preset-test] %@: %@", condition ? "PASS" : "FAIL", message)
            }

            // 1. Add via the editor path.
            guard let added = store.addPreset(command: "claude --overlay-selftest") else {
                NSLog("[preset-test] FAIL: addPreset returned nil")
                NSApp.terminate(nil)
                return
            }
            expect(
                store.mergedPresets.contains { $0.id == added.id },
                "added preset appears in merged presets"
            )
            expect(
                store.enabledPresets.contains { $0.id == added.id },
                "added preset appears in the enabled preset list"
            )

            // 2. Enable quick launch → the star sticks, siblings keep theirs,
            // and the claude quick-strip chip groups both starred presets.
            let claudeStarsBefore = store.mergedPresets
                .filter { $0.tool == .claude && $0.quickLaunch }.count
            store.updatePreset(id: added.id, quickLaunch: true)
            expect(
                store.mergedPresets.first { $0.id == added.id }?.quickLaunch == true,
                "test preset is quick-launch after update"
            )
            expect(
                store.mergedPresets.filter { $0.tool == .claude && $0.quickLaunch }.count
                    == claudeStarsBefore + 1,
                "claude siblings keep their stars (no sibling-disable)"
            )
            expect(
                store.quickPresetGroups.contains { group in
                    group.presets.contains { $0.id == added.id }
                },
                "quick strip group data now contains the test preset"
            )
            expect(
                store.quickPresetGroups.filter { $0.cli == .claude }.count <= 1,
                "starred claude presets collapse into one strip chip"
            )

            // 3. Survives a store reload (fresh instance reads the overlay).
            let reloaded = UnpeelStore()
            expect(
                reloaded.mergedPresets.first { $0.id == added.id }?.quickLaunch == true,
                "test preset survives a store reload"
            )

            // 4. Remove, then wipe the overlay key entirely: the test
            // mutations lived only in the overlay, so this restores the
            // user's original quick presets.
            store.removePreset(id: added.id)
            expect(
                !store.mergedPresets.contains { $0.id == added.id },
                "test preset removed"
            )
            AppDefaults.shared.removeObject(forKey: UnpeelStore.nativePresetsKey)
            store.rescan()
            expect(
                store.mergedPresets.contains { $0.tool == .claude && $0.quickLaunch },
                "original claude quick preset restored after overlay wipe"
            )
            expect(
                AppDefaults.shared.object(forKey: UnpeelStore.nativePresetsKey) == nil,
                "overlay key deleted (no residue)"
            )

            NSLog("[preset-test] DONE: %@", failures == 0 ? "ALL PASS" : "\(failures) FAILURE(S)")
            NSApp.terminate(nil)
        }
    }

    private static func write(window: NSWindow, to path: String) {
        // UNPEEL_SNAPSHOT_WINDOW=1: composited capture (vibrancy, traffic
        // lights) via CGWindowList — needs screen-recording permission.
        if ProcessInfo.processInfo.environment["UNPEEL_SNAPSHOT_WINDOW"] == "1",
           let cg = CGWindowListCreateImage(
               .null,
               .optionIncludingWindow,
               CGWindowID(window.windowNumber),
               [.boundsIgnoreFraming, .bestResolution]
           ), cg.width > 1 {
            let rep = NSBitmapImageRep(cgImage: cg)
            if let png = rep.representation(using: .png, properties: [:]) {
                try? png.write(to: URL(fileURLWithPath: path))
                NSLog("[UnpeelNative] composited snapshot written to \(path)")
                return
            }
        }

        guard let view = window.contentView else {
            NSLog("[UnpeelNative] snapshot: no content view")
            return
        }
        let bounds = view.bounds
        guard let rep = view.bitmapImageRepForCachingDisplay(in: bounds) else {
            NSLog("[UnpeelNative] snapshot: no bitmap rep")
            return
        }
        view.cacheDisplay(in: bounds, to: rep)
        guard let png = rep.representation(using: .png, properties: [:]) else {
            NSLog("[UnpeelNative] snapshot: png encode failed")
            return
        }
        do {
            try png.write(to: URL(fileURLWithPath: path))
            NSLog("[UnpeelNative] snapshot written to \(path)")
        } catch {
            NSLog("[UnpeelNative] snapshot write failed: \(error)")
        }
    }
}
