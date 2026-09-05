import Foundation
import XCTest
@testable import UnpeelNative

final class PlatformAdapterCallbackTests: XCTestCase {
    func testRegisteredBearerRoundTripsAcrossHookServerRestart() async throws {
        let token = HostServiceManager.platformAdapterToken()

        let first = try makeServer(token: token)
        let firstResponse = try await invoke(server: first, token: token)
        XCTAssertEqual(firstResponse.status, 200)
        XCTAssertEqual(firstResponse.body, ["ok": true, "cycle": 1])
        first.stop()

        // A replacement app-side listener receives the bearer through a new
        // registration. This exercises the real HTTP parser rather than the
        // token comparison helper that missed the Authorization-header bug.
        let second = try makeServer(token: token, cycle: 2)
        defer { second.stop() }
        let secondResponse = try await invoke(server: second, token: token)
        XCTAssertEqual(secondResponse.status, 200)
        XCTAssertEqual(secondResponse.body, ["ok": true, "cycle": 2])

        let legacyHeaderOnly = try await invoke(
            server: second,
            token: token,
            useLegacyMCPHeader: true
        )
        XCTAssertEqual(legacyHeaderOnly.status, 401)
    }

    private func makeServer(token: String, cycle: Int = 1) throws -> HookServer {
        let server = try XCTUnwrap(HookServer())
        server.platformAdapterToken = token
        server.platformAdapterHandler = { _, reply in
            reply(200, #"{"ok":true,"cycle":\#(cycle)}"#)
        }
        return server
    }

    private func invoke(
        server: HookServer,
        token: String,
        useLegacyMCPHeader: Bool = false
    ) async throws -> (status: Int, body: [String: AnyHashable]) {
        let url = try XCTUnwrap(URL(
            string: "http://127.0.0.1:\(server.port)/_unpeel/platform-adapter/call"
        ))
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.httpBody = Data(
            #"{"version":1,"operation":"computer.status","request":{}}"#.utf8
        )
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if useLegacyMCPHeader {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "x-unpeel-auth")
        } else {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.connectionProxyDictionary = [:]
        let (data, response) = try await URLSession(configuration: configuration).data(for: request)
        let http = try XCTUnwrap(response as? HTTPURLResponse)
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: AnyHashable]
        )
        return (http.statusCode, json)
    }
}
