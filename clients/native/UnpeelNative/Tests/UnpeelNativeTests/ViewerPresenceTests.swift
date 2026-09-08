import Foundation
import Testing
@testable import UnpeelNative

@MainActor
struct ViewerPresenceTests {
    @Test func oneDeviceAcrossTransportsUsesItsNewestNameAndLease() throws {
        let files = try PresenceFiles()
        let now = Date()
        try files.write([
            files.entry("Old name (phone-a)", at: now.addingTimeInterval(-5)),
            files.entry("Renamed phone (phone-a)", at: now)
        ])
        try files.write([files.entry("Phone (phone-a)", at: now.addingTimeInterval(-2))], mobile: true)
        let store = files.makeStore()
        store.refresh(now: now)

        let viewers = try #require(store.viewers["s1"])
        #expect(viewers.count == 1)
        #expect(viewers.first?.id == "device:phone-a")
        #expect(viewers.first?.displayName == "Renamed phone")
        #expect(store.isDeviceViewing(sessionID: "s1", deviceID: "phone-a"))
    }

    @Test func matchingNamesAndAddressesDoNotMergeDifferentDevices() throws {
        let files = try PresenceFiles()
        let now = Date()
        try files.write([
            files.entry("Mac (mac-a)", at: now, ip: "192.0.2.1"),
            files.entry("Mac (mac-b)", at: now, ip: "192.0.2.1"),
            files.entry(nil, at: now, ip: "192.0.2.1")
        ])
        let store = files.makeStore()
        #expect(store.viewers["s1"]?.count == 3)
        #expect(store.viewers["s1"]?.filter { $0.deviceID == nil }.count == 1)
        #expect(!store.isDeviceViewing(sessionID: "s1", deviceID: "192.0.2.1"))
        #expect(!store.hasViewers(sessionID: "another-session"))
    }

    @Test func eachTransportExpiresBeforeDevicesAreMerged() throws {
        let files = try PresenceFiles()
        let now = Date()
        try files.write([files.entry("Stream (device-a)", at: now)])
        try files.write([files.entry("Direct (device-a)", at: now.addingTimeInterval(1))], mobile: true)
        let store = files.makeStore()
        store.refresh(now: now.addingTimeInterval(17))
        #expect(store.viewers["s1"]?.first?.displayName == "Stream")
        store.refresh(now: now.addingTimeInterval(21))
        #expect(!store.hasViewers(sessionID: "s1"))
    }

    @Test func gridRepairWaitsForLastViewerAndExplicitFitThenRunsOnce() throws {
        let files = try PresenceFiles()
        let now = Date()
        try files.write([files.entry("Mac (mac-a)", at: now)])
        try files.write([files.entry("iPad (ipad-a)", at: now)], mobile: true)
        let store = files.makeStore()
        #expect(!store.consumeGridReassertCandidate("s1", hasActiveFit: false))

        try files.write([])
        store.refresh(now: now)
        #expect(store.hasViewers(sessionID: "s1"))
        #expect(!store.consumeGridReassertCandidate("s1", hasActiveFit: false))

        store.refresh(now: now.addingTimeInterval(16))
        #expect(!store.hasViewers(sessionID: "s1"))
        #expect(!store.consumeGridReassertCandidate("s1", hasActiveFit: true))
        #expect(store.consumeGridReassertCandidate("s1", hasActiveFit: false))
        #expect(!store.consumeGridReassertCandidate("s1", hasActiveFit: false))
        #expect(!store.consumeGridReassertCandidate("never-viewed", hasActiveFit: false))
    }

    @Test func transportAndSessionChangesDoNotAnnounceAnotherConnection() throws {
        let files = try PresenceFiles()
        let now = Date()
        var arrivals: [String] = []
        let store = files.makeStore { arrivals.append($0) }
        try files.write([files.entry("iPhone (phone-a)", at: now)], mobile: true)
        store.refresh(now: now)
        #expect(arrivals == ["iPhone"])

        try files.write([files.entry("Renamed iPhone (phone-a)", at: now.addingTimeInterval(1))], session: "s2")
        try files.write([], mobile: true)
        store.refresh(now: now.addingTimeInterval(1))
        #expect(arrivals == ["iPhone"])
        #expect(store.isDeviceViewing(sessionID: "s2", deviceID: "phone-a"))

        store.refresh(now: now.addingTimeInterval(22))
        try files.write([files.entry("Renamed iPhone (phone-a)", at: now.addingTimeInterval(23))])
        store.refresh(now: now.addingTimeInterval(23))
        #expect(arrivals == ["iPhone", "Renamed iPhone"])
    }

    @Test func existingViewersAreSeededWithoutAConnectionToast() throws {
        let files = try PresenceFiles()
        try files.write([files.entry("iPad (ipad-a)", at: Date())], mobile: true)
        var arrivals: [String] = []
        let store = files.makeStore { arrivals.append($0) }
        #expect(store.hasViewers(sessionID: "s1"))
        #expect(arrivals.isEmpty)
    }

    @Test func storesStayScopedToTheirOwnHostFiles() throws {
        let first = try PresenceFiles()
        let second = try PresenceFiles()
        try first.write([first.entry("iPad (ipad-a)", at: Date())], mobile: true)
        let firstStore = first.makeStore()
        let secondStore = second.makeStore()
        #expect(firstStore.hasViewers(sessionID: "s1"))
        #expect(!secondStore.hasViewers(sessionID: "s1"))
        try Data("malformed".utf8).write(to: first.directory.appendingPathComponent("mobile-presence.json"))
        firstStore.refresh()
        #expect(!firstStore.hasViewers(sessionID: "s1"))
    }
}

private final class PresenceFiles {
    let directory: URL

    init() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("presence-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    deinit { try? FileManager.default.removeItem(at: directory) }

    func entry(_ device: String?, at date: Date, ip: String? = nil) -> [String: Any] {
        var result: [String: Any] = ["last_seen": Int64(date.timeIntervalSince1970 * 1_000)]
        result["device"] = device
        result["ip"] = ip
        return result
    }

    func write(_ entries: [[String: Any]], mobile: Bool = false, session: String = "s1") throws {
        let data = try JSONSerialization.data(withJSONObject: ["version": 1, "sessions": [session: entries]])
        try data.write(to: directory.appendingPathComponent(mobile ? "mobile-presence.json" : "presence.json"))
    }

    @MainActor
    func makeStore(onConnection: ((String) -> Void)? = nil) -> ViewerPresenceStore {
        ViewerPresenceStore(
            presenceURL: directory.appendingPathComponent("presence.json"),
            automaticallyUpdates: false,
            onConnection: onConnection
        )
    }
}
