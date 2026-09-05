//
//  SurfaceCache.swift
//  UnpeelNative
//
//  Retains one GhosttyTerminalPane per session id. Surfaces are created on
//  first selection (or pre-warmed on hover intent) and then KEPT ALIVE and
//  swapped in/out of the view hierarchy — never destroyed on switch — so
//  switching sessions is instant and preserves viewport state (the native
//  advantage over the webview).
//
//  Panes are dropped when their session disappears from the store (host
//  died / GC'd), and beyond that the cache keeps only the most recently
//  shown panes (`retainedPaneLimit`) so memory doesn't grow with every
//  session ever viewed — the webview app caps retention at 2; with the
//  kqueue attach client a hidden pane costs ~0 CPU, so we can afford more.
//
//  Provider themes (OpenCode / Grok): config FSEvents + a short canvas
//  sample of output.bin (truecolor bg the TUI is painting). Sampling is
//  what makes in-TUI theme changes update the titlebar/Ghostty default bg
//  without a session switch — Grok often paints first and only later (or
//  never) writes config.toml.
//

import AppKit
import CoreServices
import Combine

/// Owns panes between cache eviction and their deferred main-loop teardown.
/// A pane requested again before teardown is reclaimed from here, so the
/// cache never starts a replacement `unpeel-attach` while the old one is
/// merely waiting for its staggered release turn.
struct DeferredSurfaceEvictions<Value> {
    struct Token: Equatable {
        fileprivate let rawValue: UInt64
    }

    private var nextToken: UInt64 = 0
    private var pending: [String: (token: Token, value: Value)] = [:]

    func contains(_ id: String) -> Bool {
        pending[id] != nil
    }

    mutating func schedule(_ value: Value, for id: String) -> Token {
        precondition(pending[id] == nil, "surface eviction already pending")
        nextToken &+= 1
        let token = Token(rawValue: nextToken)
        pending[id] = (token, value)
        return token
    }

    mutating func reclaim(_ id: String) -> Value? {
        pending.removeValue(forKey: id)?.value
    }

    /// Claim a pane for teardown only when this is still its current
    /// eviction. A reclaimed/rescheduled pane rejects stale queued work.
    mutating func take(_ id: String, token: Token) -> Value? {
        guard pending[id]?.token == token else { return nil }
        return pending.removeValue(forKey: id)?.value
    }
}

@MainActor
final class SurfaceCache: ObservableObject {
    /// Most-recently-shown panes kept beyond the selected/prewarmed set.
    static let retainedPaneLimit = 8

    /// Bumped whenever a provider frame style is reapplied. SwiftUI reads
    /// this so the titlebar and swap-container pick up the new background
    /// without a session switch.
    @Published private(set) var themeRevision: UInt = 0

    private struct PaneRecord {
        let pane: GhosttyTerminalPane
        let identity = UUID()
        var frameStyle: TerminalFrameStyle
        var command: String
        var workingDirectory: String?
        /// `app-sessions` directory this pane's `unpeel-attach` runs against:
        /// this instance's own home, or another local workspace's home when
        /// that workspace is selected in this window. Part of the identity:
        /// the same session id never appears under two homes, but a pane
        /// must never be reused across them either.
        let sessionsDir: URL
        /// Last applied style signature (config + optional canvas sample).
        var styleSignature: String
        /// Live canvas color sampled from output.bin (0xRRGGBB).
        var canvasSample: UInt32?
    }

    private var panes: [String: PaneRecord] = [:]
    private var deferredEvictions = DeferredSurfaceEvictions<PaneRecord>()

    /// Session ids in display order, most recent last (LRU bookkeeping).
    private var lastShown: [String] = []

    /// nonisolated so deinit can release it; only touched on the main queue
    /// while the cache is alive (FSEvents callback is main-queue too).
    nonisolated(unsafe) private var fsEventStream: FSEventStreamRef?
    private var watchedPaths: [String] = []
    private var themeReloadScheduled = false
    private let themeReadQueue = DispatchQueue(label: "unpeel.provider-theme", qos: .utility)
    private var themeReadInFlight = false
    private var themeReadAgain = false
    private var forceRevisionAfterRead = false
    /// nonisolated so deinit can invalidate; only touched on the main queue.
    nonisolated(unsafe) private var canvasSampleTimer: Timer?

    /// nonisolated so deinit can remove it; only touched on the main queue.
    nonisolated(unsafe) private var appTintObserver: NSObjectProtocol?

    /// nonisolated so deinit can remove it; only touched on the main queue.
    nonisolated(unsafe) private var appTintTickObserver: NSObjectProtocol?

    /// nonisolated so deinit can remove it; only touched on the main queue.
    nonisolated(unsafe) private var transparencyObserver: NSObjectProtocol?

    init() {
        rebuildThemeWatcher()
        appTintObserver = NotificationCenter.default.addObserver(
            forName: .unpeelAppTintChanged, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.reapplyAppTint()
            }
        }
        appTintTickObserver = NotificationCenter.default.addObserver(
            forName: .unpeelAppTintAnimationTick, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.reapplyAppTint(visibleOnly: true)
            }
        }
        // Appearance transparency: same re-resolve path as an App color
        // change — TerminalPaneStyle.resolved() picks up the new opacity and
        // the style signature moves, so every cached pane gets one live
        // config push. Debounced at the model, so slider drags land once.
        transparencyObserver = NotificationCenter.default.addObserver(
            forName: .unpeelTransparencyChanged, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.reapplyAppTint()
            }
        }
    }

    deinit {
        if let stream = fsEventStream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
        canvasSampleTimer?.invalidate()
        if let observer = appTintObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        if let observer = appTintTickObserver {
            NotificationCenter.default.removeObserver(observer)
        }
        if let observer = transparencyObserver {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    /// Returns the retained pane for a session, creating it on first use.
    func pane(
        for session: SessionEntry,
        workingDirectory: String?,
        sessionsDir: URL = LaunchConfig.appSessionsDir
    ) -> GhosttyTerminalPane {
        restoreDeferredPane(session.id)
        let presentationCommand = session.presentationCommand
        if let existing = panes[session.id], existing.sessionsDir != sessionsDir {
            // Defensive: a retained pane for a different home must not be
            // handed out (its attach child streams the wrong socket).
            drop(session.id)
        }
        if let existing = panes[session.id] {
            // Working directory / command can change across restart; keep
            // the record current so theme reloads resolve against the right
            // paths.
            if existing.command != presentationCommand
                || existing.workingDirectory != workingDirectory {
                let frameStyle = TerminalFrameStyle.resolved(
                    command: presentationCommand, workingDirectory: workingDirectory
                )
                panes[session.id]?.command = presentationCommand
                panes[session.id]?.workingDirectory = workingDirectory
                panes[session.id]?.canvasSample = nil
                panes[session.id]?.frameStyle = frameStyle
                panes[session.id]?.styleSignature = Self.styleSignature(
                    background: frameStyle.background, canvasSample: nil,
                    paneStyle: frameStyle.paneStyle
                )
                existing.pane.applyPaneStyle(frameStyle.paneStyle)
                rebuildThemeWatcher()
                updateCanvasSampleTimer()
                reloadProviderThemes()
            }
            return existing.pane
        }
        let sample = TerminalFrameStyle.usesProviderTheme(command: presentationCommand)
            ? ProviderCanvasSampler.cachedBackground(sessionID: session.id, sessionsDir: sessionsDir)
            : nil
        let frameStyle = TerminalFrameStyle.resolved(
            command: presentationCommand,
            workingDirectory: workingDirectory,
            canvasOverride: sample
        )
        let pane = GhosttyTerminalPane(
            command: LaunchConfig.attachCommand(
                sessionID: session.id,
                sessionsDir: sessionsDir
            ),
            workingDirectory: workingDirectory,
            sessionDirectory: sessionsDir.appendingPathComponent(
                session.id,
                isDirectory: true
            ),
            style: frameStyle.paneStyle
        )
        panes[session.id] = PaneRecord(
            pane: pane,
            frameStyle: frameStyle,
            command: presentationCommand,
            workingDirectory: workingDirectory,
            sessionsDir: sessionsDir,
            styleSignature: Self.styleSignature(
                background: frameStyle.background,
                canvasSample: sample,
                paneStyle: frameStyle.paneStyle
            ),
            canvasSample: sample
        )
        pane.onPresentationVisibilityChanged = { [weak self] in
            self?.updateCanvasSampleTimer()
            self?.reloadProviderThemes()
        }
        rebuildThemeWatcher()
        updateCanvasSampleTimer()
        return pane
    }

    func existingPane(for sessionID: String) -> GhosttyTerminalPane? {
        panes[sessionID]?.pane
    }

    /// Live canvas sample for a session, if known. Used by the titlebar path
    /// so chrome matches the retained pane without re-reading output.bin.
    func canvasSample(for sessionID: String) -> UInt32? {
        panes[sessionID]?.canvasSample
    }

    /// Resolve the current frame style for a session (config + live sample).
    func frameStyle(
        for session: SessionEntry,
        workingDirectory: String?,
        sessionsDir: URL = LaunchConfig.appSessionsDir
    ) -> TerminalFrameStyle {
        if let record = panes[session.id], record.sessionsDir == sessionsDir,
           record.command == session.presentationCommand,
           record.workingDirectory == workingDirectory {
            return record.frameStyle
        }
        return TerminalFrameStyle.resolved(
            command: session.presentationCommand,
            workingDirectory: workingDirectory
        )
    }

    /// Record that a pane was actually displayed (not just pre-warmed); the
    /// LRU keeps the most recently shown ones alive.
    func noteShown(_ sessionID: String) {
        restoreDeferredPane(sessionID)
        lastShown.removeAll { $0 == sessionID }
        lastShown.append(sessionID)
    }

    /// Drops panes whose sessions no longer exist (or exited), then trims
    /// the remainder to: the selected pane + pre-warmed panes + the most
    /// recently shown, up to `retainedPaneLimit`.
    /// `liveIDs` describes the sessions of ONE scope — the one whose
    /// `app-sessions` directory is `sessionsDir`. Panes attached to another
    /// local workspace's home are not in that set, but they are not dead
    /// either: they stay retained (LRU-bounded) so switching workspaces and
    /// back re-shows the same surface — same VT state, same grid, no
    /// replacement attach child replaying a fresh tail.
    func prune(
        keeping liveIDs: Set<String>,
        selectedID: String?,
        prewarmedIDs: [String],
        sessionsDir: URL = LaunchConfig.appSessionsDir
    ) {
        var protected = Set(prewarmedIDs)
        if let selectedID { protected.insert(selectedID) }
        // A pane can become selected/prewarmed during its stagger window.
        // Reclaim it before examining active entries so queued stale work
        // cannot tear down a pane the UI wants again.
        for id in protected {
            restoreDeferredPane(id)
        }

        var dropped = false
        for (id, record) in panes
        where record.sessionsDir == sessionsDir
            && !liveIDs.contains(id)
            && !protected.contains(id) {
            drop(id)
            dropped = true
        }

        var keep = protected
        for id in lastShown.reversed() {
            if keep.count >= Self.retainedPaneLimit { break }
            keep.insert(id)
        }
        for id in Array(panes.keys) where !keep.contains(id) {
            drop(id)
            dropped = true
        }
        if dropped {
            rebuildThemeWatcher()
            updateCanvasSampleTimer()
        }
    }

    /// Outstanding deferred teardowns (spacing factor for the next one).
    private var pendingTeardowns = 0

    /// Remove a pane from the active cache immediately, then detach/free its
    /// Ghostty EXEC surface on a staggered main-loop turn. `prune` runs inside
    /// SwiftUI's publish/layout path, where synchronous surface destruction
    /// previously froze the app. Until the queued turn starts, `pane(for:)`
    /// can reclaim this exact record, preventing a second `unpeel-attach`
    /// child from being spawned for the same session.
    private func drop(_ sessionID: String) {
        guard let record = panes.removeValue(forKey: sessionID) else { return }
        lastShown.removeAll { $0 == sessionID }
        let token = deferredEvictions.schedule(record, for: sessionID)

        pendingTeardowns += 1
        let delay = 0.1 * Double(pendingTeardowns)
        let pane = record.pane
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self else {
                // The cache died while this main-loop turn was pending. The
                // closure still owns the pane, so explicitly release its EXEC
                // surface before that final strong reference disappears.
                pane.tearDown()
                pane.removeFromSuperview()
                return
            }
            self.pendingTeardowns = max(0, self.pendingTeardowns - 1)
            guard let record = self.deferredEvictions.take(
                sessionID, token: token
            ) else {
                // Reclaimed or superseded: this queued eviction is stale.
                return
            }
            // This whole block is one main-actor turn: a replacement cannot
            // be created between claiming the record and detaching it.
            record.pane.tearDown()
            record.pane.removeFromSuperview()
        }
    }

    /// Cancel a not-yet-started deferred eviction and put the same live pane
    /// back in the active cache. The already queued closure is token-gated
    /// and becomes a harmless no-op when its turn arrives.
    private func restoreDeferredPane(_ sessionID: String) {
        guard let record = deferredEvictions.reclaim(sessionID) else { return }
        panes[sessionID] = record
        rebuildThemeWatcher()
        updateCanvasSampleTimer()
    }

    // MARK: - Live provider theme reload

    /// Collect visible provider panes' config and canvas off-main. Hidden
    /// panes refresh when presented; stale results cannot restyle a replacement.
    /// At most one batch is in flight, with one coalesced follow-up.
    func reloadProviderThemes(forceRevisionBump: Bool = false) {
        forceRevisionAfterRead = forceRevisionAfterRead || forceRevisionBump
        if themeReadInFlight {
            themeReadAgain = true
            return
        }
        let requests = panes.compactMap { id, record -> ProviderThemeReadRequest? in
            guard record.pane.isPresentedForThemeSampling,
                  TerminalFrameStyle.usesProviderTheme(command: record.command)
            else { return nil }
            return ProviderThemeReadRequest(
                sessionID: id, identity: record.identity, sessionsDir: record.sessionsDir,
                command: record.command, workingDirectory: record.workingDirectory
            )
        }
        guard !requests.isEmpty else { return }
        themeReadInFlight = true
        let forceRevision = forceRevisionAfterRead
        forceRevisionAfterRead = false
        themeReadQueue.async { [weak self] in
            let results = requests.map { ($0, $0.read()) }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.themeReadInFlight = false
                var anyChanged = false
                var applied = false
                for (request, result) in results {
                    guard var record = self.panes[request.sessionID],
                          request.matches(
                            identity: record.identity, sessionsDir: record.sessionsDir,
                            command: record.command, workingDirectory: record.workingDirectory
                          ), record.pane.isPresentedForThemeSampling
                    else { continue }
                    applied = true
                    let frameStyle = TerminalFrameStyle.resolved(
                        command: record.command, background: result.background,
                        canvasOverride: result.canvas
                    )
                    let signature = Self.styleSignature(
                        background: frameStyle.background, canvasSample: result.canvas,
                        paneStyle: frameStyle.paneStyle
                    )
                    record.canvasSample = result.canvas
                    record.frameStyle = frameStyle
                    if signature != record.styleSignature {
                        record.pane.applyPaneStyle(frameStyle.paneStyle)
                        record.styleSignature = signature
                        anyChanged = true
                    }
                    self.panes[request.sessionID] = record
                }
                if anyChanged || (forceRevision && applied) { self.themeRevision &+= 1 }
                if self.themeReadAgain {
                    self.themeReadAgain = false
                    self.reloadProviderThemes()
                }
            }
        }
    }

    /// The workspace App color settled: re-resolve changed cached pane styles
    /// (the default canvas colors are tinted inside
    /// `TerminalPaneStyle.resolved`), then bump `themeRevision` so the
    /// titlebar/swap-container chrome recomputes alongside. Duplicate scope
    /// notifications must be free: a Ghostty config apply runs synchronously
    /// on the main thread and is much more expensive than the SwiftUI wash.
    private func reapplyAppTint(visibleOnly: Bool = false) {
        var anyChanged = false
        for (id, record) in panes {
            if visibleOnly {
                // Mid-fade tick (~10Hz): only panes actually on screen, and
                // only Unpeel-themed canvases — provider-themed backgrounds
                // (OpenCode/Grok) don't follow the app tint and their
                // resolve can touch provider config on disk. Everything
                // skipped here gets the ordinary full pass at completion.
                guard record.pane.window != nil,
                      !TerminalFrameStyle.usesProviderTheme(command: record.command)
                else { continue }
            }
            let frameStyle = TerminalFrameStyle.resolved(
                command: record.command,
                workingDirectory: record.workingDirectory,
                canvasOverride: record.canvasSample
            )
            let signature = Self.styleSignature(
                background: frameStyle.background,
                canvasSample: record.canvasSample,
                paneStyle: frameStyle.paneStyle
            )
            guard signature != record.styleSignature else { continue }
            record.pane.applyPaneStyle(frameStyle.paneStyle)
            panes[id]?.styleSignature = signature
            panes[id]?.frameStyle = frameStyle
            anyChanged = true
        }
        // Mid-fade ticks skip the @Published bump: it re-renders the pane
        // view subtrees, and the completion pass bumps once anyway.
        if anyChanged, !visibleOnly {
            themeRevision &+= 1
        }
    }

    static func styleSignature(
        background: TerminalFrameStyle.Background?,
        canvasSample: UInt32?,
        paneStyle: TerminalPaneStyle
    ) -> String {
        let config = background?.signature ?? "default"
        let sample = canvasSample.map { String(format: "%06X", $0) } ?? "-"
        // The provider background alone does not describe the live Ghostty
        // theme: workspace tinting happens later in TerminalPaneStyle.resolved
        // and changes the default canvas/selection colors. Key the actual
        // color payload so a retained pane is updated just like a new one.
        let variants = [paneStyle.light, paneStyle.dark]
        let colors = variants.flatMap { variant in
            [
                variant.background,
                variant.foreground,
                variant.selectionBackground,
                variant.cursorColor,
            ] + variant.palette
        }.joined(separator: ",")
        let opacity = String(format: "%.3f", paneStyle.backgroundOpacity)
        return "\(config)|\(sample)|\(colors)|\(opacity)"
    }

    private func scheduleThemeReload() {
        guard !themeReloadScheduled else { return }
        themeReloadScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.08) { [weak self] in
            guard let self else { return }
            self.themeReloadScheduled = false
            // Config file changed — always bump so titlebar re-reads even
            // before the TUI has repainted enough for a canvas sample.
            self.reloadProviderThemes(forceRevisionBump: true)
        }
    }

    // MARK: - Canvas sampling (in-TUI theme changes)

    private func updateCanvasSampleTimer() {
        let needs = panes.values.contains {
            $0.pane.isPresentedForThemeSampling
                && TerminalFrameStyle.usesProviderTheme(command: $0.command)
        }
        if needs {
            guard canvasSampleTimer == nil else { return }
            // ~3×/sec is plenty for a theme picker; cheap (96KB file tail).
            let timer = Timer(timeInterval: 0.35, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated {
                    // Don't force a revision bump on a quiet poll — only when
                    // the sampled canvas (or config) actually moved.
                    self?.reloadProviderThemes(forceRevisionBump: false)
                }
            }
            RunLoop.main.add(timer, forMode: .common)
            canvasSampleTimer = timer
            // Immediate pass so the first paint after open catches up.
            reloadProviderThemes(forceRevisionBump: false)
        } else {
            canvasSampleTimer?.invalidate()
            canvasSampleTimer = nil
        }
    }

    // MARK: - Config FSEvents

    private func rebuildThemeWatcher() {
        let workingDirectories = panes.values.compactMap { record -> String? in
            guard TerminalFrameStyle.usesProviderTheme(command: record.command) else {
                return nil
            }
            return record.workingDirectory
        }
        let paths = ProviderThemeWatchPaths.roots(workingDirectories: workingDirectories)
        guard paths != watchedPaths || fsEventStream == nil else { return }
        watchedPaths = paths
        teardownThemeWatcher()
        guard !paths.isEmpty else { return }

        var context = FSEventStreamContext()
        context.info = Unmanaged.passUnretained(self).toOpaque()
        let callback: FSEventStreamCallback = { _, info, numEvents, eventPaths, _, _ in
            guard let info else { return }
            let cache = Unmanaged<SurfaceCache>.fromOpaque(info).takeUnretainedValue()
            // UseCFTypes → CFArray of CFString paths.
            let cfPaths = Unmanaged<CFArray>.fromOpaque(eventPaths).takeUnretainedValue()
            let paths = (cfPaths as NSArray) as? [String] ?? []
            let relevant = paths.isEmpty
                || paths.contains { ProviderThemeWatchPaths.isRelevantChange($0) }
            guard relevant else { return }
            MainActor.assumeIsolated {
                cache.scheduleThemeReload()
            }
        }
        let flags = FSEventStreamCreateFlags(
            kFSEventStreamCreateFlagFileEvents
                | kFSEventStreamCreateFlagUseCFTypes
        )
        guard let stream = FSEventStreamCreate(
            nil,
            callback,
            &context,
            paths as CFArray,
            FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
            0.2,
            flags
        ) else {
            NSLog("[UnpeelNative] provider theme FSEvents stream creation failed")
            return
        }
        FSEventStreamSetDispatchQueue(stream, .main)
        FSEventStreamStart(stream)
        fsEventStream = stream
    }

    private func teardownThemeWatcher() {
        guard let stream = fsEventStream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
        fsEventStream = nil
    }
}
