import XCTest
@testable import UnpeelNative

final class HostServiceIdentityTests: XCTestCase {
    private let own = HostServiceIdentity.Own(
        executable: "/Applications/Unpeel.app/Contents/MacOS/unpeel-host",
        version: "0.4.0",
        buildId: "1788338230.000000001:4242"
    )

    private func record(
        executable: String? = "/Applications/Unpeel.app/Contents/MacOS/unpeel-host",
        version: String? = "0.4.0",
        buildId: String? = "1788338230.000000001:4242"
    ) -> HostServiceIdentity.Record {
        HostServiceIdentity.Record(
            pid: 4242, startedAtUnixMs: 1, executable: executable,
            hostVersion: version, buildId: buildId, workspaces: nil
        )
    }

    func testMatchingServiceIsKept() {
        XCTAssertEqual(
            HostServiceIdentity.decide(record: record(), own: own, restartedThisLaunch: false),
            .keep(reason: "service matches the bundled Host")
        )
        XCTAssertEqual(
            HostServiceIdentity.decide(record: nil, own: own, restartedThisLaunch: false),
            .keep(reason: "no Host service record")
        )
    }

    func testReplacedImageOfOurOwnExecutableRestarts() {
        guard case .restart(let pid, _) = HostServiceIdentity.decide(
            record: record(buildId: "1788330000.000000000:4000"), own: own, restartedThisLaunch: false
        ) else { return XCTFail("expected restart") }
        XCTAssertEqual(pid, 4242)
    }

    func testVersionSkewRestartsInEitherDirection() {
        for version in ["0.3.1", "0.5.0"] {
            guard case .restart = HostServiceIdentity.decide(
                record: record(executable: "/opt/unpeel/bin/unpeel-host", version: version, buildId: "x"),
                own: own, restartedThisLaunch: false
            ) else { return XCTFail("expected restart for \(version)") }
        }
    }

    func testPreFourPointZeroRecordWithoutIdentityIsStale() {
        guard case .restart = HostServiceIdentity.decide(
            record: record(executable: nil, version: nil, buildId: nil), own: own, restartedThisLaunch: false
        ) else { return XCTFail("expected restart") }
    }

    func testForeignSameVersionServiceIsLeftAlone() {
        XCTAssertEqual(
            HostServiceIdentity.decide(
                record: record(executable: "/usr/local/bin/unpeel-host", buildId: "other"),
                own: own, restartedThisLaunch: false
            ),
            .keep(reason: "foreign service of the same version")
        )
    }

    func testRestartHappensAtMostOncePerLaunch() {
        guard case .keep(let reason) = HostServiceIdentity.decide(
            record: record(version: "0.3.1"), own: own, restartedThisLaunch: true
        ) else { return XCTFail("expected keep") }
        XCTAssertTrue(reason.contains("once this launch"))
    }

    func testImageTestFallsBackToProcessNameWhenThePathIsGone() {
        // A Sparkle-staged image deleted after install: no path, only a name.
        XCTAssertTrue(HostServiceIdentity.isUnpeelHostImage(path: nil, processName: "unpeel-host"))
        XCTAssertTrue(HostServiceIdentity.isUnpeelHostImage(path: "", processName: "unpeel-host"))
        XCTAssertFalse(HostServiceIdentity.isUnpeelHostImage(path: nil, processName: "zsh"))
        XCTAssertFalse(HostServiceIdentity.isUnpeelHostImage(path: nil, processName: nil))
        // A resolvable path decides on its own.
        XCTAssertTrue(HostServiceIdentity.isUnpeelHostImage(
            path: "/Applications/Unpeel.app/Contents/MacOS/unpeel-host", processName: nil))
        XCTAssertFalse(HostServiceIdentity.isUnpeelHostImage(path: "/bin/zsh", processName: "unpeel-host"))
    }

    func testProcessNameReadsTheKernelName() {
        // The test host is not unpeel-host, but the kernel name must resolve.
        let name = HostServiceIdentity.processName(getpid())
        XCTAssertNotNil(name)
        XCTAssertFalse(name?.isEmpty ?? true)
        XCTAssertNil(HostServiceIdentity.processName(-1))
    }

    func testBuildIDMatchesTheHostStampFormat() throws {
        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-build-id-\(UUID().uuidString.prefix(8))")
        try Data("abc".utf8).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let id = try XCTUnwrap(HostServiceIdentity.buildID(forExecutableAt: file.path))
        XCTAssertNotNil(id.range(of: #"^\d+\.\d{9}:3$"#, options: .regularExpression), id)
    }

    /// Process proof: a worker started from a differently-stamped copy of the
    /// Host binary is detected as skewed at "launch", restarted exactly once,
    /// and a second reconcile in the same launch leaves the replacement alone.
    func testStaleWorkerIsRestartedOnceNotInALoop() throws {
        var repo = URL(fileURLWithPath: #filePath)
        for _ in 0..<6 { repo.deleteLastPathComponent() } // …/apps/native/UnpeelNative/Tests/UnpeelNativeTests/X.swift
        let realHost = repo.appendingPathComponent("crates/target/debug/unpeel-host")
        try XCTSkipUnless(
            FileManager.default.isExecutableFile(atPath: realHost.path),
            "needs crates/target/debug/unpeel-host (cargo build -p unpeel-host)"
        )
        let scratch = URL(fileURLWithPath: "/tmp/uhsi-\(UUID().uuidString.prefix(6))")
        let home = scratch.appendingPathComponent("home")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer {
            if let record = HostServiceIdentity.readRecord(at: home.appendingPathComponent("serve.json")) {
                _ = HostServiceIdentity.terminate(pid: record.pid, startedAtUnixMs: record.startedAtUnixMs)
            }
            try? FileManager.default.removeItem(at: scratch)
        }
        // A "previous install": same binary, different stamp.
        let staleHost = scratch.appendingPathComponent("unpeel-host")
        try FileManager.default.copyItem(at: realHost, to: staleHost)
        try FileManager.default.setAttributes(
            [.modificationDate: Date(timeIntervalSince1970: 1_700_000_000)], ofItemAtPath: staleHost.path
        )
        let stale = Process()
        stale.executableURL = staleHost
        stale.arguments = ["__serve__"]
        var env = ProcessInfo.processInfo.environment
        env["UNPEEL_HOME"] = home.path
        env = env.filter { !$0.key.hasPrefix("UNPEEL_TEST_") && !$0.key.hasPrefix("UNPEEL_SNAPSHOT") }
        stale.environment = env
        stale.standardOutput = FileHandle.nullDevice
        stale.standardError = FileHandle.nullDevice
        try stale.run()
        let serveJSON = home.appendingPathComponent("serve.json")
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline, HostServiceIdentity.readRecord(at: serveJSON)?.pid != stale.processIdentifier {
            Thread.sleep(forTimeInterval: 0.1)
        }
        let record = try XCTUnwrap(HostServiceIdentity.readRecord(at: serveJSON))
        XCTAssertEqual(record.pid, stale.processIdentifier)
        XCTAssertNotEqual(record.buildId, HostServiceIdentity.buildID(forExecutableAt: realHost.path))

        HostServiceIdentity.resetLaunchGuardForTesting()
        let own = HostServiceIdentity.Own(
            executable: staleHost.path, // same executable path, replaced image
            version: record.hostVersion ?? "",
            buildId: HostServiceIdentity.buildID(forExecutableAt: realHost.path)
        )
        var log: [String] = []
        let restarted = HostServiceIdentity.reconcileAtLaunch(
            home: home, realHome: scratch.appendingPathComponent("real"), own: own, log: { log.append($0) }
        )
        XCTAssertTrue(restarted, log.joined(separator: "\n"))
        XCTAssertNotEqual(kill(stale.processIdentifier, 0), 0, "stale worker must be gone")
        XCTAssertTrue(log.contains { $0.contains("stopped the stale service") }, log.joined(separator: "\n"))

        // The replacement (started by the app in production) — here the real
        // binary — must not be restarted again during the same launch.
        let fresh = Process()
        fresh.executableURL = realHost
        fresh.arguments = ["__serve__"]
        fresh.environment = env
        fresh.standardOutput = FileHandle.nullDevice
        fresh.standardError = FileHandle.nullDevice
        try fresh.run()
        let deadline2 = Date().addingTimeInterval(20)
        while Date() < deadline2, HostServiceIdentity.readRecord(at: serveJSON)?.pid != fresh.processIdentifier {
            Thread.sleep(forTimeInterval: 0.1)
        }
        let again = HostServiceIdentity.reconcileAtLaunch(
            home: home, realHome: scratch.appendingPathComponent("real"),
            own: HostServiceIdentity.Own(executable: realHost.path, version: "9.9.9", buildId: nil),
            log: { log.append($0) }
        )
        XCTAssertFalse(again, "second reconcile in the same launch must not restart")
        XCTAssertEqual(kill(fresh.processIdentifier, 0), 0, "replacement worker must stay up")
        _ = HostServiceIdentity.terminate(pid: fresh.processIdentifier, startedAtUnixMs: HostServiceIdentity.readRecord(at: serveJSON)?.startedAtUnixMs ?? 0)
    }
}
