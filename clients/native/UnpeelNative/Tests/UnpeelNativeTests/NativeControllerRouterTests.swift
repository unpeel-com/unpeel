import Foundation
import XCTest
import UnpeelShared
@testable import UnpeelNative

final class NativeControllerRouterTests: XCTestCase {
    private let principal = NativeControllerPrincipal(deviceID: "phone-1", name: "Phone")

    func testMetricsValidationCrossesNativeBridge() {
        let result = NativeControllerRouter.shared.route(
            requestID: nil,
            method: "GET",
            path: "/mobile/metrics",
            query: [:],
            headers: [:],
            body: Data(),
            principal: principal,
            routeContext: nil
        )
        guard case .handled(let response) = result else {
            return XCTFail("expected Rust to own the metrics route, got \(result)")
        }
        XCTAssertEqual(response.status, 400)
        XCTAssertTrue(response.body.contains("invalid session id"))
    }

    func testMutatingRouteValidationCrossesNativeBridge() {
        for path in [
            "/mobile/write",
            "/mobile/resize",
            "/mobile/mark-read",
            "/mobile/request-screenshot",
            "/mobile/artifact-delete",
        ] {
            let result = NativeControllerRouter.shared.route(
                requestID: nil,
                method: "POST",
                path: path,
                query: [:],
                headers: ["content-type": "application/json"],
                body: Data(#"{}"#.utf8),
                principal: principal,
                routeContext: nil
            )
            guard case .handled(let response) = result else {
                return XCTFail("expected Rust to own \(path), got \(result)")
            }
            XCTAssertEqual(response.status, 400, path)
            XCTAssertTrue(response.body.contains("invalid session id"), path)
        }
    }

    func testArtifactListValidationCrossesNativeBridge() {
        let result = NativeControllerRouter.shared.route(
            requestID: nil,
            method: "GET",
            path: "/mobile/artifacts",
            query: [:],
            headers: [:],
            body: Data(),
            principal: principal,
            routeContext: nil
        )
        guard case .handled(let response) = result else {
            return XCTFail("expected Rust to own artifact listing, got \(result)")
        }
        XCTAssertEqual(response.status, 400)
        XCTAssertTrue(response.body.contains("invalid session id"))
    }

    func testOriginalArtifactReadCrossesNativeBridge() throws {
        let sessionID = "test-controller-artifact-\(UUID().uuidString.prefix(8))"
        let sessionRoot = LaunchConfig.appSessionsDir.appendingPathComponent(sessionID)
        let screenshots = sessionRoot
            .appendingPathComponent("artifacts/browser/screenshots", isDirectory: true)
        try FileManager.default.createDirectory(at: screenshots, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: sessionRoot) }
        try Data("0123456789".utf8).write(to: screenshots.appendingPathComponent("result.txt"))

        let result = NativeControllerRouter.shared.route(
            requestID: nil,
            method: "GET",
            path: "/mobile/artifact",
            query: [
                "session_id": sessionID,
                "kind": "screenshots",
                "name": "result.txt",
                "offset": "3",
                "limit": "4",
            ],
            headers: [:],
            body: Data(),
            principal: principal,
            routeContext: nil
        )
        guard case .handled(let response) = result else {
            return XCTFail("expected Rust to own original artifact reads, got \(result)")
        }
        XCTAssertEqual(response.status, 200)
        let body = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(response.body.utf8)) as? [String: Any]
        )
        XCTAssertEqual(body["contentType"] as? String, "text/plain; charset=utf-8")
        XCTAssertEqual(body["offset"] as? Int, 3)
        XCTAssertEqual(body["nextOffset"] as? Int, 7)
        XCTAssertEqual(body["totalSize"] as? Int, 10)
        XCTAssertEqual(
            Data(base64Encoded: try XCTUnwrap(body["dataBase64"] as? String)),
            Data("3456".utf8)
        )
    }

    func testArchiveContextCrossesNativeBridge() throws {
        let context = try JSONSerialization.data(withJSONObject: [
            "archivedSessionsByProject": [
                "project-1": [[
                    "id": "archived-1",
                    "projectID": "project-1",
                    "title": "Archived session",
                ]],
            ],
        ])
        let result = NativeControllerRouter.shared.route(
            requestID: "archive-request",
            method: "GET",
            path: "/mobile/archive",
            query: ["project_id": "project-1"],
            headers: [:],
            body: Data(),
            principal: principal,
            routeContext: context
        )
        guard case .handled(let response) = result else {
            return XCTFail("expected Rust to own archive listing, got \(result)")
        }
        XCTAssertEqual(response.status, 200)
        let body = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(response.body.utf8)) as? [String: Any]
        )
        XCTAssertEqual(body["projectID"] as? String, "project-1")
        let sessions = try XCTUnwrap(body["sessions"] as? [[String: Any]])
        XCTAssertEqual(sessions.first?["id"] as? String, "archived-1")
    }

    func testUnknownRouteReturnsUnhandled() {
        let result = NativeControllerRouter.shared.route(
            requestID: nil,
            method: "GET",
            path: "/mobile/not-migrated",
            query: [:],
            headers: [:],
            body: Data(),
            principal: principal,
            routeContext: nil
        )
        XCTAssertEqual(result, .unhandled)
    }

    func testBinaryBodyUsesBase64Envelope() {
        let result = NativeControllerRouter.shared.route(
            requestID: nil,
            method: "POST",
            path: "/mobile/not-migrated",
            query: [:],
            headers: ["content-type": "application/octet-stream"],
            body: Data([0xFF, 0x00, 0x80]),
            principal: principal,
            routeContext: nil
        )
        XCTAssertEqual(result, .unhandled)
    }
}

final class NativeControllerServerBoundaryTests: XCTestCase {
    private final class MemoryE2EKeyStore: MobileE2EKeyStoring {
        private var values: [String: Data] = [:]

        func load(deviceID: String) -> Data? { values[deviceID] }
        func save(_ key: Data, deviceID: String) throws { values[deviceID] = key }
        func delete(deviceID: String) { values.removeValue(forKey: deviceID) }
    }

    private final class RecordingControllerRouter: NativeControllerRouting, @unchecked Sendable {
        struct Call: Sendable {
            let requestID: String?
            let method: String
            let path: String
            let principal: NativeControllerPrincipal
        }

        private let lock = NSLock()
        private let result: NativeControllerRouteResult
        private var recordedCalls: [Call] = []

        init(result: NativeControllerRouteResult) {
            self.result = result
        }

        var calls: [Call] {
            lock.withLock { recordedCalls }
        }

        func route(
            requestID: String?,
            method: String,
            path: String,
            query _: [String: String],
            headers _: [String: String],
            body _: Data,
            principal: NativeControllerPrincipal,
            routeContext _: Data?
        ) -> NativeControllerRouteResult {
            lock.withLock {
                recordedCalls.append(Call(
                    requestID: requestID,
                    method: method,
                    path: path,
                    principal: principal
                ))
            }
            return result
        }
    }

    private final class LockedFlag: @unchecked Sendable {
        private let lock = NSLock()
        private var value = false

        func set() { lock.withLock { value = true } }
        var isSet: Bool { lock.withLock { value } }
    }

    private final class LockedString: @unchecked Sendable {
        private let lock = NSLock()
        private var value: String?

        func set(_ newValue: String) { lock.withLock { value = newValue } }
        var current: String? { lock.withLock { value } }
    }

    private final class LockedPrincipal: @unchecked Sendable {
        private let lock = NSLock()
        private var value: NativeControllerPrincipal?

        func set(_ newValue: NativeControllerPrincipal) { lock.withLock { value = newValue } }
        var current: NativeControllerPrincipal? { lock.withLock { value } }
    }

    private final class LockedCounter: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0

        @discardableResult
        func increment() -> Int {
            lock.withLock {
                value += 1
                return value
            }
        }

        var current: Int { lock.withLock { value } }
    }

    private actor AsyncGate {
        private var isOpen = false
        private var waiters: [CheckedContinuation<Void, Never>] = []

        func wait() async {
            if isOpen { return }
            await withCheckedContinuation { continuation in
                waiters.append(continuation)
            }
        }

        func open() {
            guard !isOpen else { return }
            isOpen = true
            let pending = waiters
            waiters.removeAll()
            for waiter in pending { waiter.resume() }
        }
    }
}
