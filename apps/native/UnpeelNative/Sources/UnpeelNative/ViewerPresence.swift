//
//  ViewerPresence.swift
//  UnpeelNative
//
//  Tracks which other devices are currently viewing a session's terminal, so
//  the title bar can show small presence avatars (ViewerAvatarsView).
//
//  Two feeds converge here:
//
//  - File feed: the Rust remote server (crates/unpeel-core/src/remote_server.rs)
//    writes `~/.unpeel/remote/presence.json` whenever remote viewers change:
//    {"version":1,"updated_at":ms,"sessions":{"<id>":[{"ip","kind":"ws"|"poll",
//    "device":"Name (id)"|null,"last_seen":ms}]}}. Poll viewers have a 15s TTL
//    server-side, so entries whose last_seen is older than ~20s are treated as
//    stale on read. The file is watched with a DispatchSource on the remote/
//    directory plus a low-frequency fallback timer (the file — or the whole
//    directory — may not exist yet).
//
//  - Mobile feed: the Rust workspace worker publishes authenticated Direct/
//    Link output leases beside it as `mobile-presence.json` (same TTL rules).
//

import Foundation

/// One device currently viewing a session's terminal.
struct ViewerInfo: Identifiable, Equatable {
    enum Kind: Equatable {
        /// A paired phone on the worker's Direct/Link output lease.
        case mobile
        /// A client of the crate remote server (ws or poll).
        case remote
    }

    let id: String
    /// Stable paired-device id when this viewer is an authenticated
    /// Controller. Keeping it separate from the display label lets push
    /// suppression target only the phone that is actually watching instead
    /// of silencing every paired phone (or a remote Mac) at once.
    let deviceID: String?
    let displayName: String
    let kind: Kind
    let lastSeen: Date
}

@MainActor
final class ViewerPresenceStore: ObservableObject {
    static let shared = ViewerPresenceStore()

    /// Session id → current viewers, already de-staled and sorted.
    @Published private(set) var viewers: [String: [ViewerInfo]] = [:]

    /// File-feed staleness cutoff. The remote server prunes poll viewers after
    /// 15s; anything older than this on disk is a leftover from a dead server.
    private static let fileEntryTTL: TimeInterval = 20
    /// In-app mobile feed TTL — matches the remote server's poll-viewer TTL.
    private static let mobileEntryTTL: TimeInterval = 15
    private static let pollInterval: TimeInterval = 5

    private let presenceURL: URL
    /// Authenticated Direct/Link output leases published by the canonical
    /// Rust workspace worker. It intentionally sits beside `presence.json`
    /// so the same directory watcher covers both terminal data planes.
    private let mobilePresenceURL: URL
    private var fileViewers: [String: [ViewerInfo]] = [:]
    private var mobileFileViewers: [String: [ViewerInfo]] = [:]

    private var directoryWatcher: DispatchSourceFileSystemObject?
    private var pollTimer: Timer?

    init(presenceURL: URL = LaunchConfig.unpeelDir
        .appendingPathComponent("remote")
        .appendingPathComponent("presence.json")
    ) {
        self.presenceURL = presenceURL
        mobilePresenceURL = presenceURL.deletingLastPathComponent()
            .appendingPathComponent("mobile-presence.json")
        reloadPresenceFile()
        startDirectoryWatcher()
        // Low-frequency fallback: re-reads the file (covers a missed fs event
        // or a remote/ dir created after launch) and prunes expired entries.
        let timer = Timer(timeInterval: Self.pollInterval, repeats: true) { _ in
            Task { @MainActor in
                ViewerPresenceStore.shared.refresh()
            }
        }
        timer.tolerance = 1
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
    }

    /// True when a paired phone is actively viewing this session — via either
    /// transport (the in-app `/mobile/output` feed OR the WS `__remote__`
    /// server's `presence.json`). The activity engine uses it to avoid
    /// re-marking a remotely-watched session unread the moment it settles.
    /// Reads the merged, TTL-pruned `viewers` so both feeds are covered.
    func hasLiveMobileViewer(sessionID: String) -> Bool {
        !(viewers[sessionID]?.isEmpty ?? true)
    }

    /// Whether one exact paired Controller is currently rendering this
    /// session. Phone pushes are fanned out per target, so a foreground iPad
    /// must not suppress a background iPhone, and a remote Mac must not
    /// suppress either one.
    func isDeviceViewing(sessionID: String, deviceID: String) -> Bool {
        viewers[sessionID]?.contains { $0.deviceID == deviceID } == true
    }

    /// Session ids a remote viewer has been seen on at some point this app
    /// run. A remote controller can resize the *shared hosted PTY* while the
    /// Mac's own surface stays put (no local resize event ever fires), so
    /// the desktop keeps rendering the diverged grid. The desktop grid
    /// re-assert (`TerminalArea.normalizeShownTerminalSize`) consumes one
    /// candidacy per session to run the forced resize path exactly once,
    /// keeping ordinary never-remote-viewed switches free of refit churn.
    private var gridReassertCandidates: Set<String> = []

    /// One-shot: true if `sessionID` had a remote viewer since the last
    /// consume (or app launch). Callers force a full grid re-assert on true.
    func consumeGridReassertCandidate(_ sessionID: String) -> Bool {
        gridReassertCandidates.remove(sessionID) != nil
    }

    // MARK: - Refresh / prune

    private func refresh() {
        if directoryWatcher == nil {
            startDirectoryWatcher()
        }
        reloadPresenceFile()
    }

    private func rebuild(now: Date = Date()) {
        var merged: [String: [ViewerInfo]] = [:]
        for (sessionID, entries) in fileViewers {
            let live = entries.filter {
                now.timeIntervalSince($0.lastSeen) <= Self.fileEntryTTL
            }
            guard !live.isEmpty else { continue }
            var combined = merged[sessionID] ?? []
            for entry in live where !combined.contains(where: { $0.id == entry.id }) {
                combined.append(entry)
            }
            merged[sessionID] = combined
        }
        for (sessionID, entries) in mobileFileViewers {
            let live = entries.filter {
                now.timeIntervalSince($0.lastSeen) <= Self.mobileEntryTTL
            }
            guard !live.isEmpty else { continue }
            var combined = merged[sessionID] ?? []
            for entry in live where !combined.contains(where: { $0.id == entry.id }) {
                combined.append(entry)
            }
            merged[sessionID] = combined
        }
        for (sessionID, list) in merged {
            merged[sessionID] = list.sorted { lhs, rhs in
                lhs.displayName.localizedCaseInsensitiveCompare(rhs.displayName)
                    == .orderedAscending
                    || (lhs.displayName == rhs.displayName && lhs.id < rhs.id)
            }
        }
        // Latch before publishing: whoever is viewing now may resize the
        // shared PTY at any point while present, so candidacy is set on
        // sight and only cleared by the consuming re-assert.
        gridReassertCandidates.formUnion(merged.keys)
        if merged != viewers {
            viewers = merged
        }
        announceConnectionChanges(in: merged)
    }

    /// Device ids currently present, so a phone appearing (across any session,
    /// either transport) fires a one-shot "connected" toast rather than the
    /// only cue being the small title-bar avatar chips. Reconnect after the
    /// device drops re-announces.
    private var announcedDeviceIDs: Set<String> = []

    private func announceConnectionChanges(in merged: [String: [ViewerInfo]]) {
        var live: [String: String] = [:]  // viewer id → display name
        for list in merged.values {
            for viewer in list {
                live[viewer.id] = viewer.displayName
            }
        }
        let liveIDs = Set(live.keys)
        // Suppress the initial population (app just launched with a phone
        // already viewing) — only announce genuinely new arrivals.
        if !didSeedAnnouncedDevices {
            announcedDeviceIDs = liveIDs
            didSeedAnnouncedDevices = true
            return
        }
        for id in liveIDs.subtracting(announcedDeviceIDs) {
            let name = live[id] ?? "A device"
            ToastCenter.shared.show("\(name) connected", systemImage: "iphone.radiowaves.left.and.right")
        }
        announcedDeviceIDs = liveIDs
    }

    private var didSeedAnnouncedDevices = false

    // MARK: - File feed (presence.json)

    private func reloadPresenceFile() {
        if let data = try? Data(contentsOf: presenceURL) {
            fileViewers = Self.parsePresence(data: data, kind: .remote)
        } else if !fileViewers.isEmpty {
            // Missing/unreadable file simply means "no remote viewers".
            fileViewers = [:]
        }
        if let data = try? Data(contentsOf: mobilePresenceURL) {
            mobileFileViewers = Self.parsePresence(data: data, kind: .mobile)
        } else if !mobileFileViewers.isEmpty {
            mobileFileViewers = [:]
        }
        rebuild()
    }

    private static func parsePresence(
        data: Data,
        kind: ViewerInfo.Kind
    ) -> [String: [ViewerInfo]] {
        struct PresenceFile: Decodable {
            let sessions: [String: [PresenceEntry]]?
        }
        struct PresenceEntry: Decodable {
            let ip: String?
            let kind: String?
            let device: String?
            let lastSeen: Int64?

            enum CodingKeys: String, CodingKey {
                case ip, kind, device
                case lastSeen = "last_seen"
            }
        }
        guard let file = try? JSONDecoder().decode(PresenceFile.self, from: data),
              let sessions = file.sessions
        else { return [:] }

        var result: [String: [ViewerInfo]] = [:]
        for (sessionID, entries) in sessions {
            var list: [ViewerInfo] = []
            for entry in entries {
                let identity = entry.device ?? entry.ip ?? "remote"
                let viewer = ViewerInfo(
                    id: "\(kind == .mobile ? "mobile" : "remote"):\(identity)",
                    deviceID: deviceID(fromDevice: entry.device),
                    displayName: displayName(fromDevice: entry.device, ip: entry.ip),
                    kind: kind,
                    lastSeen: Date(
                        timeIntervalSince1970: Double(entry.lastSeen ?? 0) / 1000
                    )
                )
                if !list.contains(where: { $0.id == viewer.id }) {
                    list.append(viewer)
                }
            }
            if !list.isEmpty { result[sessionID] = list }
        }
        return result
    }

    /// The remote server records `device` as "Name (id)"; show just the name.
    private static func displayName(fromDevice device: String?, ip: String?) -> String {
        if let device, !device.isEmpty {
            if device.hasSuffix(")"), let open = device.range(of: " (", options: .backwards) {
                let name = String(device[..<open.lowerBound])
                if !name.isEmpty { return name }
            }
            return device
        }
        return ip ?? "Remote viewer"
    }

    /// The remote server records authenticated Controllers as "Name (id)".
    /// An IP-only/legacy viewer has no stable device identity and therefore
    /// cannot suppress a particular phone's APNs target.
    private static func deviceID(fromDevice device: String?) -> String? {
        guard let device, device.hasSuffix(")"),
              let open = device.range(of: " (", options: .backwards)
        else { return nil }
        let start = open.upperBound
        let end = device.index(before: device.endIndex)
        guard start < end else { return nil }
        let id = String(device[start..<end])
        return id.isEmpty ? nil : id
    }

    // MARK: - Directory watcher

    private func startDirectoryWatcher() {
        let directory = presenceURL.deletingLastPathComponent()
        let fd = open(directory.path, O_EVTONLY)
        guard fd >= 0 else { return } // retried from the fallback timer
        let source = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd,
            eventMask: [.write, .delete, .rename],
            queue: .global(qos: .utility)
        )
        // @Sendable: these closures are formed in a @MainActor context but run
        // on the source's utility queue — without it they inherit MainActor
        // isolation and the runtime's executor check crashes the app.
        source.setEventHandler(handler: { @Sendable in
            Task { @MainActor in
                ViewerPresenceStore.shared.handleDirectoryEvent()
            }
        })
        source.setCancelHandler(handler: { @Sendable in close(fd) })
        source.resume()
        directoryWatcher = source
    }

    private func handleDirectoryEvent() {
        // If the directory itself was replaced, re-arm the watcher.
        if !FileManager.default.fileExists(
            atPath: presenceURL.deletingLastPathComponent().path
        ) {
            directoryWatcher?.cancel()
            directoryWatcher = nil
        }
        reloadPresenceFile()
    }
}
