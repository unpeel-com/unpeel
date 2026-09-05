//
//  HookServer.swift
//  UnpeelNative
//
//  The app's loopback listener. It is NOT a Host: provider hooks, `/mcp/*`,
//  `/notify/*`, hosted-App, and mobile routes belong to the workspace worker
//  (`unpeel serve`) and answer 404 here. This listener:
//
//  - listens on 127.0.0.1 with an OS-assigned port (port 0),
//  - registers that port in ~/.unpeel/app-ports on startup and removes it
//    on clean shutdown (peers and the worker's ownership probe find it
//    there; every response carries `X-Unpeel-Controller-Owner: serve`),
//  - serves the worker's authenticated platform-adapter callback
//    (`/_unpeel/platform-adapter/call`, per-process bearer registered over
//    `host.sock`),
//  - serves the three frontend coordination pings (`/state-changed`,
//    `/show-window`, `/reload-appearance`).
//
//  Transport is a plain BSD socket with a blocking accept loop and one
//  thread per connection, the same structure as the Rust server
//  (TcpListener + thread-per-stream with a 5s read timeout). An earlier
//  Network.framework (NWListener) variant intermittently sat on delivered
//  bytes until the peer closed, which made hook curls (--max-time 2) time
//  out; blocking reads have no such failure mode.
//

import CoreFoundation
import Darwin
import Foundation

enum StrictHTTPContentLengthError: Error, Equatable {
    case invalid
    case tooLarge
}

/// The native listeners support fixed-length request bodies only. Parse that
/// framing header strictly so signed, overflowed, empty, or comma-joined
/// values cannot reach Data indexing with an invalid offset.
enum StrictHTTPContentLength {
    static let maximum = 4 * 1024 * 1024

    static func parse(_ raw: String?) throws -> Int {
        guard let raw else { return 0 }
        guard !raw.isEmpty,
              raw.utf8.allSatisfy({ (48...57).contains($0) }),
              let value = Int(raw)
        else { throw StrictHTTPContentLengthError.invalid }
        guard value <= maximum else { throw StrictHTTPContentLengthError.tooLarge }
        return value
    }
}

/// One Host-owned approval row mirrored for native presentation.
struct PlatformPresentedApproval: Equatable {
    let id: String
    let kind: String
    let title: String
    let body: String
    let callerSessionID: String
    let targetSessionID: String?
    let requestedAtUnixMs: Int64
}

enum PlatformAdapterCall: Equatable {
    case presentApprovals([PlatformPresentedApproval])
    case openInEditor(path: String)
    case thumbnail(query: [String: String])
    case computerStatus
    case refreshLinkEntitlement(macID: String)
    case reconcileMobileE2EKeys
    case removeMobileE2EKey(deviceID: String)
    case overlaySnapshot
    case setProjectFolderColor(projectID: String, colorID: String?)
    case registerPushToken(deviceID: String, token: String, environment: String)
    case recoverRelayCredentials(deviceID: String)
    case setNotifyWhenDone(sessionID: String, enabled: Bool)
    case deliverNotification(
        sessionID: String,
        title: String,
        body: String,
        kind: String,
        requiresNotifyWhenDone: Bool,
        sendDesktop: Bool,
        suppressDeviceIDs: [String]
    )
}

enum PlatformAdapterCallError: Error, Equatable {
    case invalidEnvelope
    case unsupportedOperation
}

/// One parsed hook or App-alert POST. Provider field names match the JSON the
/// hook scripts send and the Rust listener contract.
final class HookServer: @unchecked Sendable {
    private(set) var port: UInt16 = 0
    private var listenFD: Int32 = -1
    private var acceptThread: Thread?

    /// Native-only operations invoked by the canonical Rust workspace worker.
    /// Registration travels over mode-0600 `host.sock`; this callback remains
    /// loopback-only and requires the per-process bearer registered there.
    private let platformAdapterTokenLock = NSLock()
    private var storedPlatformAdapterToken: String?
    var platformAdapterToken: String? {
        get {
            platformAdapterTokenLock.lock()
            defer { platformAdapterTokenLock.unlock() }
            return storedPlatformAdapterToken
        }
        set {
            platformAdapterTokenLock.lock()
            storedPlatformAdapterToken = newValue
            platformAdapterTokenLock.unlock()
        }
    }
    var platformAdapterHandler: (@Sendable (
        _ body: Data,
        _ reply: @escaping @Sendable (Int, String) -> Void
    ) -> Void)?

    /// Handler for `/state-changed`: another Unpeel wrote shared state and
    /// wants this one to re-read it now. Unauthenticated by design — it
    /// carries no data and only ever costs a rescan, exactly like the hook
    /// events arriving on the same loopback port.
    var stateChangeHandler: (@Sendable (_ change: String) -> Void)?

    /// Handler for `/show-window`: a peer instance (the sidebar workspace
    /// selector in another workspace's app) asks this instance to surface its
    /// main window — programmatic NSRunningApplication activation cannot
    /// reopen a windowless app. Unauthenticated like `/state-changed`: no
    /// data, and the only effect is showing a window the user asked for.
    var showWindowHandler: (@Sendable () -> Void)?

    /// Handler for `/reload-appearance`: a peer instance changed this
    /// workspace's stored App color (its per-line color picker wrote our
    /// defaults suite) and asks us to re-read and repaint. Unauthenticated
    /// like `/state-changed`: no data, only a local re-read.
    var reloadAppearanceHandler: (@Sendable () -> Void)?

    private static let debug = ProcessInfo.processInfo.environment["UNPEEL_DEBUG"] == "1"

    /// Binds 127.0.0.1:0, starts the accept loop, and registers the
    /// assigned port in ~/.unpeel/app-ports. Returns nil if the socket
    /// could not be created.
    init?() {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }

        // Close-on-exec so spawned children (session hosts, gateways) can
        // never inherit the listener and keep the port bound past app death.
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)

        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = 0 // OS-assigned
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")

        let bound = withUnsafePointer(to: &addr) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, listen(fd, 16) == 0 else {
            close(fd)
            return nil
        }

        var assigned = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let got = withUnsafeMutablePointer(to: &assigned) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(fd, $0, &length)
            }
        }
        guard got == 0 else {
            close(fd)
            return nil
        }

        listenFD = fd
        port = UInt16(bigEndian: assigned.sin_port)

        let thread = Thread { [weak self] in
            self?.acceptLoop(fd: fd)
        }
        thread.name = "unpeel.hook-server"
        thread.start()
        acceptThread = thread

        Self.registerPort(port)
        NSLog("[UnpeelNative] loopback listener on 127.0.0.1:%d", Int(port))
    }

    /// Closes the listener and removes our port from ~/.unpeel/app-ports.
    /// Call from applicationWillTerminate for a clean shutdown.
    func stop() {
        if listenFD >= 0 {
            close(listenFD)
            listenFD = -1
        }
        if port != 0 {
            Self.unregisterPort(port)
            NSLog("[UnpeelNative] hook server stopped, port %d unregistered", Int(port))
            port = 0
        }
    }

    // MARK: - Accept / connection threads (hook_server.rs:583-589)

    private func acceptLoop(fd: Int32) {
        while true {
            let client = accept(fd, nil, nil)
            guard client >= 0 else {
                // EBADF after stop(); transient errors just retry.
                if errno == EBADF || errno == ECONNABORTED && listenFD < 0 { return }
                if listenFD < 0 { return }
                continue
            }
            let thread = Thread { [weak self] in
                self?.handleConnection(client)
            }
            thread.start()
        }
    }

    private func handleConnection(_ fd: Int32) {
        defer { close(fd) }

        // 5s read timeout like the Rust server (handle_connection :1432).
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 64 * 1024)
        var headerEndRange: Range<Data.Index>?
        var contentLength = 0
        var contentLengthHeader: String?
        var hasTransferEncoding = false
        var requestLine = ""
        var authorizationHeader: String?

        // Read headers, then exactly content-length body bytes.
        while true {
            if headerEndRange == nil,
               let range = buffer.range(of: Data("\r\n\r\n".utf8)) {
                headerEndRange = range
                guard let header = String(
                    data: buffer[buffer.startIndex..<range.lowerBound], encoding: .utf8
                ) else {
                    respond(fd, status: 400, body: #"{"error":"bad request"}"#)
                    return
                }
                let lines = header.components(separatedBy: "\r\n")
                requestLine = lines.first ?? ""
                for line in lines.dropFirst() {
                    if line.lowercased().hasPrefix("content-length:") {
                        guard contentLengthHeader == nil else {
                            respond(fd, status: 400, body: #"{"error":"duplicate content-length"}"#)
                            return
                        }
                        contentLengthHeader = line.dropFirst("content-length:".count)
                            .trimmingCharacters(in: .whitespaces)
                    }
                    if line.lowercased().hasPrefix("transfer-encoding:") {
                        hasTransferEncoding = true
                    }
                    if line.lowercased().hasPrefix("authorization:") {
                        authorizationHeader = line.split(separator: ":", maxSplits: 1)
                            .dropFirst().first
                            .map { $0.trimmingCharacters(in: .whitespaces) }
                    }
                }
                if hasTransferEncoding {
                    respond(fd, status: 400, body: #"{"error":"unsupported transfer-encoding"}"#)
                    return
                }
                do {
                    contentLength = try StrictHTTPContentLength.parse(contentLengthHeader)
                } catch StrictHTTPContentLengthError.tooLarge {
                    respond(fd, status: 400, body: #"{"error":"body too large"}"#)
                    return
                } catch {
                    respond(fd, status: 400, body: #"{"error":"invalid content-length"}"#)
                    return
                }
            }

            if let headerEnd = headerEndRange {
                let bodyBytes = buffer.distance(from: headerEnd.upperBound, to: buffer.endIndex)
                if bodyBytes >= contentLength {
                    let body = buffer.subdata(
                        in: headerEnd.upperBound..<buffer.index(headerEnd.upperBound, offsetBy: contentLength)
                    )
                    handleRequest(
                        fd,
                        requestLine: requestLine,
                        body: body,
                        authorizationHeader: authorizationHeader
                    )
                    return
                }
            }

            if buffer.count > 8 * 1024 * 1024 { return }
            let read = recv(fd, &chunk, chunk.count, 0)
            guard read > 0 else { return } // timeout, error, or peer closed early
            buffer.append(contentsOf: chunk[0..<read])
        }
    }

    // MARK: - Request handling (hook_server.rs handle_connection :1431-1570)

    private func handleRequest(
        _ fd: Int32,
        requestLine: String,
        body: Data,
        authorizationHeader: String?
    ) {
        let parts = requestLine.split(separator: " ")
        guard parts.count >= 2 else {
            respond(fd, status: 400, body: #"{"error":"bad request"}"#)
            return
        }
        let method = String(parts[0])
        let path = String(parts[1])

        // hook_server.rs:1441-1444 — POST only.
        guard method == "POST" else {
            respond(fd, status: 405, body: #"{"error":"method not allowed"}"#)
            return
        }

        // The app is a Controller plus a connection-scoped platform adapter.
        // Provider hooks, MCP, and hosted-App routes belong only to the
        // canonical worker; this listener retains the three frontend
        // coordination pings and the authenticated adapter callback.
        guard Self.shouldDispatch(path) else {
            respond(fd, status: 404, body: #"{"error":"not found"}"#)
            return
        }

        // hook_server.rs:1476-1482 — body must be JSON.
        guard let json = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] else {
            respond(fd, status: 400, body: #"{"error":"invalid json"}"#)
            return
        }

        if path == "/_unpeel/platform-adapter/call" {
            guard Self.platformAdapterAuthorizationMatches(
                authorizationHeader,
                token: platformAdapterToken
            ) else {
                respond(fd, status: 401, body: #"{"error":"unauthorized"}"#)
                return
            }
            guard let handler = platformAdapterHandler else {
                respond(fd, status: 503, body: #"{"error":"adapter unavailable"}"#)
                return
            }
            let done = DispatchSemaphore(value: 0)
            let box = ResultBox()
            handler(body) { status, body in
                box.set((status, body))
                done.signal()
            }
            let callbackCeiling: TimeInterval = (json["operation"] as? String)
                == "link.entitlement.refresh" ? 18 : 4
            _ = done.wait(timeout: .now() + callbackCeiling)
            let (status, responseBody) = box.get()
                ?? (504, #"{"error":"platform adapter timed out"}"#)
            respond(fd, status: status, body: responseBody)
            return
        }

        // Route: /state-changed — the cross-frontend refresh ping
        // (unpeel-core state_bus). The TUI, the CLI and this app are peers
        // on the same bus; whoever writes shared state tells the others.
        if path == "/state-changed" {
            let change = (try? JSONSerialization.jsonObject(with: body) as? [String: Any])
                .flatMap { $0?["change"] as? String } ?? "unknown"
            stateChangeHandler?(change)
            respond(fd, status: 200, body: #"{"ok":true}"#)
            return
        }

        // Route: /show-window — a peer workspace instance surfaces this one
        // (sidebar workspace selector switch to a windowless running app).
        if path == "/show-window" {
            showWindowHandler?()
            respond(fd, status: 200, body: #"{"ok":true}"#)
            return
        }

        // Route: /reload-appearance — a peer changed this workspace's App
        // color from its per-line picker; re-read the stored tint.
        if path == "/reload-appearance" {
            reloadAppearanceHandler?()
            respond(fd, status: 200, body: #"{"ok":true}"#)
            return
        }

        respond(fd, status: 404, body: #"{"error":"not found"}"#)
    }

    /// The exact loopback surface. Every historical Host route (provider
    /// hooks, `/mcp/*`, `/notify/*`, hosted-App context/theme/opener, and
    /// the mobile routes) answers 404: `unpeel serve` owns them.
    static func shouldDispatch(_ path: String) -> Bool {
        switch path {
        case "/_unpeel/platform-adapter/call",
             "/state-changed",
             "/show-window",
             "/reload-appearance":
            return true
        default:
            return false
        }
    }


    static func editorPath(from json: [String: Any]) -> String? {
        guard let rawPath = json["path"] as? String,
              !rawPath.isEmpty,
              rawPath.utf8.count <= 16_384,
              NSString(string: rawPath).isAbsolutePath
        else { return nil }

        let standardized = URL(fileURLWithPath: rawPath).standardizedFileURL.path
        guard FileManager.default.fileExists(atPath: standardized) else { return nil }
        return standardized
    }

    /// Exact bearer match without early content-dependent exit. The route is
    /// loopback-only, but the secret still guards unrelated local processes.
    static func platformAdapterAuthorizationMatches(_ header: String?, token: String?) -> Bool {
        guard let token, token.utf8.count >= 32,
              let header,
              header.count > 7,
              header.prefix(7).lowercased() == "bearer "
        else { return false }
        let supplied = Array(header.dropFirst(7).utf8)
        let expected = Array(token.utf8)
        var difference = UInt64(supplied.count) ^ UInt64(expected.count)
        let count = max(supplied.count, expected.count)
        for index in 0..<count {
            let left = index < supplied.count ? supplied[index] : 0
            let right = index < expected.count ? expected[index] : 0
            difference |= UInt64(left ^ right)
        }
        return difference == 0
    }

    /// Decode the bounded callback envelope emitted by `unpeel serve`.
    /// Operation discovery remains Host-owned; this parser accepts only the
    /// native implementation registered by this app process.
    static func platformAdapterCall(
        from body: Data
    ) -> Result<PlatformAdapterCall, PlatformAdapterCallError> {
        guard let envelope = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              envelope["version"] as? Int == 1,
              let operation = envelope["operation"] as? String
        else { return .failure(.invalidEnvelope) }
        guard let request = envelope["request"] as? [String: Any] else {
            return .failure(.invalidEnvelope)
        }
        switch operation {
        case "approval.present":
            guard let rawApprovals = request["approvals"] as? [Any],
                  rawApprovals.count <= 32
            else { return .failure(.invalidEnvelope) }
            var approvals: [PlatformPresentedApproval] = []
            var seen = Set<String>()
            for raw in rawApprovals {
                guard let value = raw as? [String: Any],
                      let id = value["id"] as? String,
                      let kind = value["kind"] as? String,
                      let title = value["title"] as? String,
                      let bodyText = value["body"] as? String,
                      let callerSessionID = value["callerSessionID"] as? String,
                      let requested = value["requestedAtUnixMs"] as? NSNumber,
                      ["write", "browser", "computer", "app-open"].contains(kind),
                      !id.isEmpty,
                      id.utf8.count <= 128,
                      seen.insert(id).inserted,
                      !title.isEmpty,
                      title.utf8.count <= 1_024,
                      !bodyText.isEmpty,
                      bodyText.utf8.count <= 4_096,
                      !callerSessionID.isEmpty,
                      callerSessionID.utf8.count <= 128,
                      CFGetTypeID(requested) != CFBooleanGetTypeID(),
                      requested.int64Value >= 0
                else { return .failure(.invalidEnvelope) }
                let target = value["targetSessionID"] as? String
                guard target == nil || !(target?.isEmpty ?? true),
                      (target?.utf8.count ?? 0) <= 128
                else { return .failure(.invalidEnvelope) }
                approvals.append(PlatformPresentedApproval(
                    id: id,
                    kind: kind,
                    title: title,
                    body: bodyText,
                    callerSessionID: callerSessionID,
                    targetSessionID: target,
                    requestedAtUnixMs: requested.int64Value
                ))
            }
            return .success(.presentApprovals(approvals))
        case "app.open-in-editor":
            guard request.count == 1,
                  let path = editorPath(from: request)
            else { return .failure(.invalidEnvelope) }
            return .success(.openInEditor(path: path))
        case "artifact.thumbnail":
            guard request.count == 1,
                  let rawQuery = request["query"] as? [String: Any],
                  rawQuery.count <= 7
            else { return .failure(.invalidEnvelope) }
            let allowed = Set([
                "session_id", "sessionID", "kind", "name", "offset", "limit", "max_dim",
            ])
            var query: [String: String] = [:]
            for (key, rawValue) in rawQuery {
                guard allowed.contains(key),
                      let value = rawValue as? String,
                      !value.isEmpty,
                      value.utf8.count <= 16_384
                else { return .failure(.invalidEnvelope) }
                query[key] = value
            }
            guard query["session_id"] != nil || query["sessionID"] != nil,
                  query["kind"] != nil,
                  query["name"] != nil,
                  query["max_dim"].flatMap(Int.init).map({ $0 > 0 }) == true
            else { return .failure(.invalidEnvelope) }
            for key in ["offset", "limit"] {
                if let value = query[key], UInt64(value) == nil {
                    return .failure(.invalidEnvelope)
                }
            }
            return .success(.thumbnail(query: query))
        case "computer.status":
            guard request.isEmpty else { return .failure(.invalidEnvelope) }
            return .success(.computerStatus)
        case "link.entitlement.refresh":
            guard request.count == 1,
                  let rawMacID = request["macID"] as? String
            else { return .failure(.invalidEnvelope) }
            let macID = rawMacID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !macID.isEmpty,
                  macID.utf8.count <= 256,
                  !macID.contains("\0"),
                  !macID.contains("\n"),
                  !macID.contains("\r")
            else { return .failure(.invalidEnvelope) }
            return .success(.refreshLinkEntitlement(macID: macID))
        case "mobile.e2e-key.reconcile":
            guard let action = request["action"] as? String else {
                return .failure(.invalidEnvelope)
            }
            if action == "sync", request.count == 1 {
                return .success(.reconcileMobileE2EKeys)
            }
            guard action == "remove",
                  request.count == 2,
                  let rawDeviceID = request["deviceID"] as? String
            else { return .failure(.invalidEnvelope) }
            let deviceID = rawDeviceID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !deviceID.isEmpty,
                  deviceID.utf8.count <= 256,
                  !deviceID.contains("\0"),
                  !deviceID.contains("\n"),
                  !deviceID.contains("\r")
            else { return .failure(.invalidEnvelope) }
            return .success(.removeMobileE2EKey(deviceID: deviceID))
        case "overlay.snapshot":
            guard request.isEmpty else { return .failure(.invalidEnvelope) }
            return .success(.overlaySnapshot)
        case "overlay.project-color.set":
            guard request.count == 2,
                  let rawProjectID = request["projectID"] as? String,
                  let colorID = request["colorID"] as? String
            else { return .failure(.invalidEnvelope) }
            let projectID = rawProjectID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !projectID.isEmpty,
                  projectID.utf8.count <= 256,
                  !projectID.contains("\0"),
                  !projectID.contains("\n"),
                  !projectID.contains("\r"),
                  colorID.isEmpty || ProjectFolderColor(rawValue: colorID) != nil
            else { return .failure(.invalidEnvelope) }
            return .success(.setProjectFolderColor(
                projectID: projectID,
                colorID: colorID.isEmpty ? nil : colorID
            ))
        case "push.register":
            guard let rawDeviceID = request["deviceID"] as? String,
                  let token = request["apnsToken"] as? String,
                  let environment = request["environment"] as? String
            else { return .failure(.invalidEnvelope) }
            let deviceID = rawDeviceID.trimmingCharacters(in: .whitespacesAndNewlines)
            let tokenBytes = token.utf8
            guard !deviceID.isEmpty,
                  deviceID.utf8.count <= 256,
                  !deviceID.contains("\0"),
                  !deviceID.contains("\n"),
                  !deviceID.contains("\r"),
                  (16...200).contains(tokenBytes.count),
                  tokenBytes.allSatisfy({ byte in
                      (48...57).contains(byte) || (65...70).contains(byte)
                          || (97...102).contains(byte)
                  }),
                  environment == "sandbox" || environment == "production"
            else { return .failure(.invalidEnvelope) }
            return .success(.registerPushToken(
                deviceID: deviceID,
                token: token,
                environment: environment
            ))
        case "relay.credentials.recover":
            guard let rawDeviceID = request["deviceID"] as? String else {
                return .failure(.invalidEnvelope)
            }
            let deviceID = rawDeviceID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !deviceID.isEmpty,
                  deviceID.utf8.count <= 256,
                  !deviceID.contains("\0"),
                  !deviceID.contains("\n"),
                  !deviceID.contains("\r")
            else { return .failure(.invalidEnvelope) }
            return .success(.recoverRelayCredentials(deviceID: deviceID))
        case "session.notify_when_done.set":
            guard let rawSessionID = request["sessionID"] as? String,
                  let rawEnabled = request["notifyWhenDone"],
                  CFGetTypeID(rawEnabled as CFTypeRef) == CFBooleanGetTypeID(),
                  let enabled = rawEnabled as? Bool
            else { return .failure(.invalidEnvelope) }
            let sessionID = rawSessionID.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !sessionID.isEmpty,
                  sessionID.utf8.count <= 128,
                  !sessionID.contains("/"),
                  !sessionID.contains("..")
            else { return .failure(.invalidEnvelope) }
            return .success(.setNotifyWhenDone(sessionID: sessionID, enabled: enabled))
        case "notification.deliver":
            guard let rawSessionID = request["sessionID"] as? String,
                  let rawTitle = request["title"] as? String,
                  let rawBody = request["body"] as? String,
                  let kind = request["kind"] as? String,
                  let rawRequires = request["requiresNotifyWhenDone"],
                  CFGetTypeID(rawRequires as CFTypeRef) == CFBooleanGetTypeID(),
                  let requiresNotifyWhenDone = rawRequires as? Bool,
                  let rawSendDesktop = request["sendDesktop"],
                  CFGetTypeID(rawSendDesktop as CFTypeRef) == CFBooleanGetTypeID(),
                  let sendDesktop = rawSendDesktop as? Bool,
                  let rawSuppressed = request["suppressDeviceIDs"] as? [Any]
            else { return .failure(.invalidEnvelope) }
            let sessionID = rawSessionID.trimmingCharacters(in: .whitespacesAndNewlines)
            let title = rawTitle.trimmingCharacters(in: .whitespacesAndNewlines)
            let bodyText = rawBody.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !sessionID.isEmpty,
                  sessionID.utf8.count <= 128,
                  !sessionID.contains("/"),
                  !sessionID.contains(".."),
                  !title.isEmpty,
                  title.utf8.count <= 512,
                  !bodyText.isEmpty,
                  bodyText.utf8.count <= 4_096,
                  ["needs_input", "done", "alert"].contains(kind),
                  rawSuppressed.count <= 64
            else { return .failure(.invalidEnvelope) }
            var suppressed: [String] = []
            var seen = Set<String>()
            for raw in rawSuppressed {
                guard let rawID = raw as? String else { return .failure(.invalidEnvelope) }
                let id = rawID.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !id.isEmpty,
                      id.utf8.count <= 256,
                      !id.contains("\0"),
                      !id.contains("\n"),
                      !id.contains("\r"),
                      seen.insert(id).inserted
                else { return .failure(.invalidEnvelope) }
                suppressed.append(id)
            }
            return .success(.deliverNotification(
                sessionID: sessionID,
                title: title,
                body: bodyText,
                kind: kind,
                requiresNotifyWhenDone: requiresNotifyWhenDone,
                sendDesktop: sendDesktop,
                suppressDeviceIDs: suppressed
            ))
        default:
            return .failure(.unsupportedOperation)
        }
    }

    private func respond(_ fd: Int32, status: Int, body: String) {
        let reason: String
        switch status {
        case 200: reason = "OK"
        case 400: reason = "Bad Request"
        case 401: reason = "Unauthorized"
        case 404: reason = "Not Found"
        case 405: reason = "Method Not Allowed"
        case 504: reason = "Gateway Timeout"
        default: reason = "Error"
        }
        let payload =
            "HTTP/1.1 \(status) \(reason)\r\n"
            + "Content-Type: application/json\r\n"
            + "X-Unpeel-Frontend: native\r\n"
            + "X-Unpeel-Controller-Owner: \(LocalHostClientFeature.controllerOwnerHeaderValue)\r\n"
            + "Content-Length: \(body.utf8.count)\r\n"
            + "Connection: close\r\n\r\n"
            + body
        let bytes = Array(payload.utf8)
        var sent = 0
        while sent < bytes.count {
            let n = bytes.withUnsafeBufferPointer { pointer in
                send(fd, pointer.baseAddress! + sent, bytes.count - sent, 0)
            }
            guard n > 0 else { return }
            sent += n
        }
    }

    // MARK: - Session ownership (session_activity.rs is_known_session)

    /// A session is "known" when its hosted manifest exists on disk. The
    /// native store rebuilds all of its session state from these manifests,
    /// so the file check is authoritative (same fallback the Rust server
    /// uses, session_activity.rs:493-499).
    static func isKnownSession(_ sessionID: String) -> Bool {
        guard !sessionID.contains("/"), !sessionID.contains("..") else { return false }
        let manifest = LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent("manifest.json")
        return FileManager.default.fileExists(atPath: manifest.path)
    }

    // MARK: - Port registry (~/.unpeel/app-ports)

    static var portRegistryURL: URL {
        LaunchConfig.unpeelDir.appendingPathComponent("app-ports")
    }

    private static var portRegistryLockURL: URL {
        LaunchConfig.unpeelDir.appendingPathComponent("app-ports.lock")
    }

    /// Reads one port per line; unparsable lines are dropped.
    static func readPortRegistry() -> [UInt16] {
        guard let raw = try? String(contentsOf: portRegistryURL, encoding: .utf8) else {
            return []
        }
        return raw.split(whereSeparator: \.isNewline).compactMap {
            UInt16($0.trimmingCharacters(in: .whitespaces))
        }
    }

    /// Writes ports as newline-joined values with a trailing newline; the file
    /// is removed when no ports remain.
    private static func writePortRegistry(_ ports: [UInt16]) {
        let url = portRegistryURL
        if ports.isEmpty {
            _ = unlink(url.path)
            return
        }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let body = ports.map(String.init).joined(separator: "\n") + "\n"
        guard let data = body.data(using: .utf8) else { return }
        let temporary = url.deletingLastPathComponent().appendingPathComponent(
            ".app-ports.\(getpid()).\(UUID().uuidString).tmp"
        )
        let descriptor = open(temporary.path, O_CREAT | O_EXCL | O_WRONLY, mode_t(0o600))
        guard descriptor >= 0 else { return }
        var succeeded = true
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            var offset = 0
            while offset < raw.count {
                let count = Darwin.write(
                    descriptor,
                    base.advanced(by: offset),
                    raw.count - offset
                )
                if count <= 0 {
                    succeeded = false
                    return
                }
                offset += count
            }
        }
        if fsync(descriptor) != 0 { succeeded = false }
        close(descriptor)
        if succeeded, rename(temporary.path, url.path) == 0 {
            return
        }
        _ = unlink(temporary.path)
    }

    private static func withPortRegistryLock(_ body: () -> Void) {
        try? FileManager.default.createDirectory(
            at: portRegistryURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let descriptor = open(
            portRegistryLockURL.path,
            O_CREAT | O_RDWR,
            mode_t(0o600)
        )
        guard descriptor >= 0 else { return }
        defer { close(descriptor) }
        guard fchmod(descriptor, mode_t(0o600)) == 0,
              flock(descriptor, LOCK_EX) == 0
        else { return }
        defer { _ = flock(descriptor, LOCK_UN) }
        body()
    }

    /// A refused loopback connection proves there is no listener behind a
    /// registry entry. Every other outcome is deliberately retained: a busy
    /// listener or a local resource error must not make us evict a live peer.
    private static func isDefinitelyStalePort(_ port: UInt16) -> Bool {
        let descriptor = socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else { return false }
        defer { close(descriptor) }
        _ = fcntl(descriptor, F_SETFD, FD_CLOEXEC)

        let flags = fcntl(descriptor, F_GETFL, 0)
        guard flags >= 0,
              fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) == 0
        else { return false }

        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = inet_addr("127.0.0.1")

        let connected = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if connected == 0 { return false }
        if errno == ECONNREFUSED { return true }
        guard errno == EINPROGRESS || errno == EWOULDBLOCK else { return false }

        var pollDescriptor = pollfd(
            fd: descriptor,
            events: Int16(POLLOUT),
            revents: 0
        )
        guard poll(&pollDescriptor, 1, 20) > 0 else { return false }
        var socketError: Int32 = 0
        var length = socklen_t(MemoryLayout<Int32>.size)
        guard getsockopt(
            descriptor, SOL_SOCKET, SO_ERROR, &socketError, &length
        ) == 0 else { return false }
        return socketError == ECONNREFUSED
    }

    /// Pure registry reconciliation kept visible to focused unit tests. The
    /// caller supplies the liveness predicate so tests never touch real ports.
    static func reconciledPortRegistry(
        _ existing: [UInt16],
        registering port: UInt16,
        isDefinitelyStale: (UInt16) -> Bool
    ) -> [UInt16] {
        var seen = Set<UInt16>()
        var ports = existing.filter { candidate in
            candidate != 0
                && candidate != port
                && seen.insert(candidate).inserted
                && !isDefinitelyStale(candidate)
        }
        ports.append(port)
        let maxEntries = 16
        if ports.count > maxEntries {
            ports.removeFirst(ports.count - maxEntries)
        }
        return ports
    }

    /// Prunes provably dead listeners, dedupes this server's port, appends it
    /// last, and caps the registry at 16 entries (oldest dropped). This keeps
    /// dev rebuilds or crashed frontends from making every state announcement
    /// fan out to a full registry of dead sockets.
    private static func registerPort(_ port: UInt16) {
        withPortRegistryLock {
            let ports = reconciledPortRegistry(
                readPortRegistry(),
                registering: port,
                isDefinitelyStale: isDefinitelyStalePort
            )
            writePortRegistry(ports)
        }
    }

    /// Removes this server's port from the registry.
    private static func unregisterPort(_ port: UInt16) {
        withPortRegistryLock {
            var ports = readPortRegistry()
            ports.removeAll { $0 == port }
            writePortRegistry(ports)
        }
    }

    // MARK: - Trace log (~/.unpeel/hooks/trace.log, shared with Tauri)

    private static func trace(_ line: String) {
        let url = LaunchConfig.unpeelDir
            .appendingPathComponent("hooks")
            .appendingPathComponent("trace.log")
        let stamped = "\(UInt64(Date().timeIntervalSince1970 * 1000)) \(line)\n"
        guard let data = stamped.data(using: .utf8) else { return }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        if let handle = try? FileHandle(forWritingTo: url) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: data)
        } else {
            try? data.write(to: url)
        }
    }
}

/// Tiny lock box so the connection thread can wait on a main-actor reply
/// without capturing a mutable local in a @Sendable closure.
private final class ResultBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: (Int, String)?

    func set(_ newValue: (Int, String)) {
        lock.lock()
        value = newValue
        lock.unlock()
    }

    func get() -> (Int, String)? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}
