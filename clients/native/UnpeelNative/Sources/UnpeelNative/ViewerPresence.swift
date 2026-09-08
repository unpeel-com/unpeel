//
//  ViewerPresence.swift
//  UnpeelNative
//
//  Tracks which other devices are currently viewing a session's terminal, so
//  pane headers can show presence chips (ViewerAvatarsView). Presence is
//  device-level observation, not human membership or a terminal control lease.
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
    let id: String
    /// Stable paired-device id when this viewer is an authenticated
    /// Controller. Keeping it separate from the display label lets push
    /// suppression target only the phone that is actually watching instead
    /// of silencing every paired phone (or a remote Mac) at once.
    let deviceID: String?
    let displayName: String
    let lastSeen: Date
}

@MainActor
final class ViewerPresenceStore: ObservableObject {
    static let shared = ViewerPresenceStore(onConnection: { name in
        ToastCenter.shared.show("\(name) connected", systemImage: "person.crop.circle.badge.checkmark")
    })

    /// Session id → current viewers, already de-staled and sorted.
    @Published private(set) var viewers: [String: [ViewerInfo]] = [:]

    /// File-feed staleness cutoff. The remote server prunes poll viewers after
    /// 15s; anything older than this on disk is a leftover from a dead server.
    private static let fileEntryTTL: TimeInterval = 20
    /// Host Direct/Link output lease TTL. The legacy filename is mobile,
    /// but this feed also carries paired Mac Controllers.
    private static let mobileEntryTTL: TimeInterval = 15
    private static let pollInterval: TimeInterval = 5

    private let presenceURL: URL
    /// Authenticated Direct/Link output leases published by the canonical
    /// Rust workspace worker. It intentionally sits beside `presence.json`
    /// so the same directory watcher covers both terminal data planes.
    private let mobilePresenceURL: URL
    private var fileViewers: [String: [ViewerInfo]] = [:]
    private var mobileFileViewers: [String: [ViewerInfo]] = [:]

    private let observation = PresenceObservation()
    private let automaticallyUpdates: Bool
    private let onConnection: ((String) -> Void)?

    init(presenceURL: URL = LaunchConfig.unpeelDir
        .appendingPathComponent("remote")
        .appendingPathComponent("presence.json"),
        automaticallyUpdates: Bool = true,
        onConnection: ((String) -> Void)? = nil
    ) {
        self.presenceURL = presenceURL
        self.automaticallyUpdates = automaticallyUpdates
        self.onConnection = onConnection
        mobilePresenceURL = presenceURL.deletingLastPathComponent()
            .appendingPathComponent("mobile-presence.json")
        reloadPresenceFile()
        guard automaticallyUpdates else { return }
        startDirectoryWatcher()
        // Low-frequency fallback: re-reads the file (covers a missed fs event
        // or a remote/ dir created after launch) and prunes expired entries.
        let timer = Timer(timeInterval: Self.pollInterval, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.refresh()
            }
        }
        timer.tolerance = 1
        RunLoop.main.add(timer, forMode: .common)
        observation.timer = timer
    }

    /// Both output feeds count as presence, including mobile viewers that
    /// may resize the shared PTY. Observation alone does not prove who sized it.
    func hasViewers(sessionID: String) -> Bool {
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

    /// Preserve the repair until all viewers have left and the Host's
    /// explicit fit has cleared. A present device must never lose its grid
    /// merely because another viewer disconnected.
    func consumeGridReassertCandidate(_ sessionID: String, hasActiveFit: Bool) -> Bool {
        guard !hasActiveFit, !hasViewers(sessionID: sessionID) else { return false }
        return gridReassertCandidates.remove(sessionID) != nil
    }

    // MARK: - Refresh / prune

    func refresh(now: Date = Date()) {
        if automaticallyUpdates && observation.directoryWatcher == nil {
            startDirectoryWatcher()
        }
        reloadPresenceFile(now: now)
    }

    private func rebuild(now: Date = Date()) {
        var bySession: [String: [String: ViewerInfo]] = [:]
        // Expire each source before merging: a stale lease in one transport
        // must not hide the same device's live lease in the other.
        for (feed, ttl) in [
            (fileViewers, Self.fileEntryTTL),
            (mobileFileViewers, Self.mobileEntryTTL)
        ] {
            for (sessionID, entries) in feed {
                for entry in entries where now.timeIntervalSince(entry.lastSeen) <= ttl {
                    if let previous = bySession[sessionID]?[entry.id],
                       previous.lastSeen >= entry.lastSeen { continue }
                    bySession[sessionID, default: [:]][entry.id] = entry
                }
            }
        }
        let merged = bySession.mapValues { entries in
            entries.values.sorted { lhs, rhs in
                let order = lhs.displayName.localizedCaseInsensitiveCompare(rhs.displayName)
                return order == .orderedSame ? lhs.id < rhs.id : order == .orderedAscending
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

    /// Device ids currently present, so a viewer appearing (across any session,
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
        for id in liveIDs.subtracting(announcedDeviceIDs).sorted() {
            onConnection?(live[id] ?? "A device")
        }
        announcedDeviceIDs = liveIDs
    }

    private var didSeedAnnouncedDevices = false

    // MARK: - File feed (presence.json)

    private func reloadPresenceFile(now: Date = Date()) {
        if let data = try? Data(contentsOf: presenceURL) {
            fileViewers = Self.parsePresence(data: data, source: "terminal")
        } else if !fileViewers.isEmpty {
            // Missing/unreadable file simply means "no remote viewers".
            fileViewers = [:]
        }
        if let data = try? Data(contentsOf: mobilePresenceURL) {
            mobileFileViewers = Self.parsePresence(data: data, source: "direct-link")
        } else if !mobileFileViewers.isEmpty {
            mobileFileViewers = [:]
        }
        rebuild(now: now)
    }

    private static func parsePresence(
        data: Data,
        source: String
    ) -> [String: [ViewerInfo]] {
        struct PresenceFile: Decodable {
            let sessions: [String: [PresenceEntry]]?
        }
        struct PresenceEntry: Decodable {
            let ip: String?
            let device: String?
            let lastSeen: Int64?

            enum CodingKeys: String, CodingKey {
                case ip, device
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
                let deviceID = deviceID(fromDevice: entry.device)
                let viewer = ViewerInfo(
                    id: deviceID.map { "device:\($0)" } ?? "legacy:\(source):\(identity)",
                    deviceID: deviceID,
                    displayName: displayName(fromDevice: entry.device, ip: entry.ip),
                    lastSeen: Date(
                        timeIntervalSince1970: Double(entry.lastSeen ?? 0) / 1000
                    )
                )
                // Keep duplicates until the timestamp-aware merge. The first
                // connection in the file may be older than another live one.
                list.append(viewer)
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
        source.setEventHandler(handler: { @Sendable [weak self] in
            Task { @MainActor [weak self] in
                self?.handleDirectoryEvent()
            }
        })
        source.setCancelHandler(handler: { @Sendable in close(fd) })
        source.resume()
        observation.directoryWatcher = source
    }

    private func handleDirectoryEvent() {
        // If the directory itself was replaced, re-arm the watcher.
        if !FileManager.default.fileExists(
            atPath: presenceURL.deletingLastPathComponent().path
        ) {
            observation.directoryWatcher?.cancel()
            observation.directoryWatcher = nil
        }
        reloadPresenceFile()
    }
}

/// Resource lifetime is local to a store. Test stores can disable observation;
/// future per-Host stores must not leave timers or file descriptors behind.
private final class PresenceObservation {
    var timer: Timer?
    var directoryWatcher: DispatchSourceFileSystemObject?

    deinit {
        timer?.invalidate()
        directoryWatcher?.cancel()
    }
}
