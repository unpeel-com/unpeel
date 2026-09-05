import Darwin
import Foundation

/// Controller-owned transport for adding a phone to an already-selected
/// remote Host. It exposes exactly one short-lived, opaque exchange and owns
/// no Host state: the selected Host still validates the sealed request and
/// mints every durable credential.
final class ControllerPairingProxy: @unchecked Sendable {
    typealias Provider = @MainActor @Sendable (Data) async throws -> Data

    struct Reservation: Sendable {
        let id: String
        let endpoint: URL
    }

    private struct ActiveReservation {
        let id: String
        let expiresAt: Date
        let provider: Provider
        var forwarding = false
    }

    private let stateLock = NSLock()
    private let boundPort: UInt16
    private let advertisedHostOverride: String?
    private var listenFD: Int32
    private var activeReservation: ActiveReservation?

    /// Bind one ephemeral LAN listener. Production resolves the current LAN
    /// address each time a QR is minted; tests may advertise loopback while
    /// still exercising the real socket and HTTP exchange.
    init?(advertisedHost: String? = nil) {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)

        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = 0
        address.sin_addr.s_addr = inet_addr("0.0.0.0")
        let bound = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, listen(fd, 4) == 0 else {
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

        boundPort = UInt16(bigEndian: assigned.sin_port)
        advertisedHostOverride = advertisedHost
        listenFD = fd

        let thread = Thread { [weak self] in
            self?.acceptLoop(fd: fd)
        }
        thread.name = "unpeel.controller-pairing-proxy"
        thread.start()
    }

    deinit {
        stop()
    }

    func stop() {
        let fd = withControllerPairingProxyLock(stateLock) { () -> Int32 in
            activeReservation = nil
            let current = listenFD
            listenFD = -1
            return current
        }
        guard fd >= 0 else { return }
        _ = shutdown(fd, SHUT_RDWR)
        close(fd)
    }

    /// Replace any prior invitation with one random, short-lived URL. The
    /// extra 30 seconds matches the historical proxy grace beyond the QR's
    /// five-minute expiry.
    func reserve(provider: @escaping Provider) -> Reservation? {
        let id = UUID().uuidString.uppercased()
        let host = advertisedHostOverride ?? LocalNetworkAddress.preferredIPv4()
        guard let endpoint = URL(
            string: "http://\(host):\(boundPort)/mobile/pairing-proxy/\(id)"
        ) else { return nil }
        let accepted = withControllerPairingProxyLock(stateLock) { () -> Bool in
            guard listenFD >= 0 else { return false }
            activeReservation = ActiveReservation(
                id: id,
                expiresAt: Date().addingTimeInterval(5 * 60 + 30),
                provider: provider
            )
            return true
        }
        return accepted ? Reservation(id: id, endpoint: endpoint) : nil
    }

    func cancel(id: String) {
        withControllerPairingProxyLock(stateLock) {
            if activeReservation?.id == id {
                activeReservation = nil
            }
        }
    }

    private func acceptLoop(fd: Int32) {
        while true {
            let client = accept(fd, nil, nil)
            guard client >= 0 else {
                let stillListening = withControllerPairingProxyLock(stateLock) {
                    listenFD == fd
                }
                if !stillListening || errno == EBADF || errno == EINVAL { return }
                continue
            }
            var noDelay: Int32 = 1
            setsockopt(
                client,
                IPPROTO_TCP,
                TCP_NODELAY,
                &noDelay,
                socklen_t(MemoryLayout<Int32>.size)
            )
            let thread = Thread { [self] in
                handleConnection(client)
            }
            thread.start()
        }
    }

    private func handleConnection(_ fd: Int32) {
        defer { close(fd) }
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        do {
            let request = try readRequest(fd)
            let body = try forward(method: request.method, path: request.path, body: request.body)
            respond(fd, status: 200, body: body)
        } catch let error as MobileRemoteError {
            respond(fd, status: error.status, body: errorJSON(error.message))
        } catch {
            respond(fd, status: 400, body: errorJSON("bad request"))
        }
    }

    private struct Request {
        let method: String
        let path: String
        let body: Data
    }

    private func readRequest(_ fd: Int32) throws -> Request {
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 16 * 1024)
        var headerEnd: Data.Index?
        var contentLength = 0
        var requestLine = ""

        while true {
            if headerEnd == nil,
               let range = buffer.range(of: Data("\r\n\r\n".utf8)) {
                headerEnd = range.upperBound
                guard let header = String(
                    data: buffer[buffer.startIndex..<range.lowerBound],
                    encoding: .utf8
                ) else {
                    throw MobileRemoteError(400, "bad request")
                }
                let lines = header.components(separatedBy: "\r\n")
                requestLine = lines.first ?? ""
                var contentLengthHeader: String?
                for line in lines.dropFirst() {
                    guard let split = line.firstIndex(of: ":") else { continue }
                    let name = line[..<split]
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                        .lowercased()
                    let value = line[line.index(after: split)...]
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                    if name == "content-length" {
                        guard contentLengthHeader == nil else {
                            throw MobileRemoteError(400, "duplicate content-length")
                        }
                        contentLengthHeader = value
                    } else if name == "transfer-encoding" {
                        throw MobileRemoteError(400, "unsupported transfer-encoding")
                    }
                }
                do {
                    contentLength = try StrictHTTPContentLength.parse(contentLengthHeader)
                } catch StrictHTTPContentLengthError.tooLarge {
                    throw MobileRemoteError(400, "body too large")
                } catch {
                    throw MobileRemoteError(400, "invalid content-length")
                }
            }

            if let headerEnd {
                let bodyBytes = buffer.distance(from: headerEnd, to: buffer.endIndex)
                if bodyBytes >= contentLength {
                    let bodyEnd = buffer.index(headerEnd, offsetBy: contentLength)
                    let parts = requestLine.split(separator: " ")
                    guard parts.count >= 2,
                          let components = URLComponents(
                            string: "http://unpeel.local\(parts[1])"
                          ),
                          components.query == nil,
                          components.fragment == nil
                    else {
                        throw MobileRemoteError(400, "bad request")
                    }
                    return Request(
                        method: String(parts[0]),
                        path: components.path,
                        body: buffer.subdata(in: headerEnd..<bodyEnd)
                    )
                }
            }

            guard buffer.count <= StrictHTTPContentLength.maximum + 64 * 1024 else {
                throw MobileRemoteError(400, "request too large")
            }
            let count = recv(fd, &chunk, chunk.count, 0)
            guard count > 0 else { throw MobileRemoteError(400, "bad request") }
            buffer.append(contentsOf: chunk[0..<count])
        }
    }

    private func forward(method: String, path: String, body: Data) throws -> String {
        let parts = path.split(separator: "/", omittingEmptySubsequences: true)
        guard parts.count == 4,
              parts[0] == "mobile",
              parts[1] == "pairing-proxy",
              parts[3] == "pair"
        else {
            throw MobileRemoteError(404, "not found")
        }
        guard method == "POST" else {
            throw MobileRemoteError(405, "method not allowed")
        }

        let id = String(parts[2])
        let reservation = withControllerPairingProxyLock(stateLock) {
            () -> ActiveReservation? in
            guard let activeReservation,
                  activeReservation.id == id,
                  activeReservation.expiresAt > Date(),
                  !activeReservation.forwarding
            else {
                if self.activeReservation?.expiresAt ?? .distantPast <= Date() {
                    self.activeReservation = nil
                }
                return nil
            }
            self.activeReservation?.forwarding = true
            return activeReservation
        }
        guard let reservation else {
            throw MobileRemoteError(410, "pairing invitation expired")
        }

        let response: Data
        do {
            response = try mainActorAsyncValue {
                try await reservation.provider(body)
            }
        } catch let error as MobileRemoteError {
            releaseForwardingClaim(id: id)
            throw error
        } catch {
            releaseForwardingClaim(id: id)
            throw MobileRemoteError(502, error.localizedDescription)
        }
        guard let responseBody = String(data: response, encoding: .utf8) else {
            releaseForwardingClaim(id: id)
            throw MobileRemoteError(502, "Host returned an invalid pairing response")
        }
        cancel(id: id)
        return responseBody
    }

    private func releaseForwardingClaim(id: String) {
        withControllerPairingProxyLock(stateLock) {
            if activeReservation?.id == id {
                activeReservation?.forwarding = false
            }
        }
    }

    private func mainActorAsyncValue<T>(
        _ operation: @escaping @MainActor @Sendable () async throws -> T
    ) throws -> T {
        let semaphore = DispatchSemaphore(value: 0)
        let box = ControllerPairingProxyResultBox<Result<T, Error>>()
        Task { @MainActor in
            do {
                box.set(.success(try await operation()))
            } catch {
                box.set(.failure(error))
            }
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + 20) == .timedOut {
            throw MobileRemoteError(504, "pairing proxy timed out")
        }
        switch box.get() {
        case let .success(value): return value
        case let .failure(error): throw error
        case nil: throw MobileRemoteError(504, "pairing proxy timed out")
        }
    }

    private func errorJSON(_ message: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: ["error": message]))
            ?? Data(#"{"error":"request failed"}"#.utf8)
        return String(data: data, encoding: .utf8) ?? #"{"error":"request failed"}"#
    }

    private func respond(_ fd: Int32, status: Int, body: String) {
        let reason: String
        switch status {
        case 200: reason = "OK"
        case 400: reason = "Bad Request"
        case 404: reason = "Not Found"
        case 405: reason = "Method Not Allowed"
        case 410: reason = "Gone"
        case 502: reason = "Bad Gateway"
        case 504: reason = "Gateway Timeout"
        default: reason = "Error"
        }
        let payload =
            "HTTP/1.1 \(status) \(reason)\r\n"
            + "Content-Type: application/json\r\n"
            + "Cache-Control: no-store\r\n"
            + "Content-Length: \(body.utf8.count)\r\n"
            + "Connection: close\r\n\r\n"
            + body
        let bytes = Array(payload.utf8)
        var sent = 0
        while sent < bytes.count {
            let count = bytes.withUnsafeBufferPointer { pointer in
                send(fd, pointer.baseAddress! + sent, bytes.count - sent, 0)
            }
            guard count > 0 else { return }
            sent += count
        }
    }
}

enum LocalNetworkAddress {
    static func preferredIPv4() -> String {
        var ifaddr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddr) == 0, let first = ifaddr else {
            return "127.0.0.1"
        }
        defer { freeifaddrs(ifaddr) }

        let skippedPrefixes = ["lo", "utun", "awdl", "llw", "bridge"]
        var candidates: [(interfaceName: String, address: String)] = []
        for pointer in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let interface = pointer.pointee
            guard let address = interface.ifa_addr,
                  address.pointee.sa_family == UInt8(AF_INET)
            else { continue }
            let name = String(cString: interface.ifa_name)
            if skippedPrefixes.contains(where: { name.hasPrefix($0) }) { continue }
            let flags = Int32(interface.ifa_flags)
            guard flags & IFF_UP != 0, flags & IFF_RUNNING != 0 else { continue }

            var ipv4 = address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                $0.pointee.sin_addr
            }
            var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            guard inet_ntop(AF_INET, &ipv4, &buffer, socklen_t(INET_ADDRSTRLEN)) != nil else {
                continue
            }
            let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
            let value = String(decoding: bytes, as: UTF8.self)
            if !value.hasPrefix("127.") {
                candidates.append((interfaceName: name, address: value))
            }
        }
        return selectPreferredIPv4(candidates) ?? "127.0.0.1"
    }

    static func selectPreferredIPv4(
        _ candidates: [(interfaceName: String, address: String)]
    ) -> String? {
        candidates.min { lhs, rhs in
            let lhsRank = interfaceRank(lhs.interfaceName)
            let rhsRank = interfaceRank(rhs.interfaceName)
            if lhsRank != rhsRank { return lhsRank < rhsRank }
            if lhs.interfaceName != rhs.interfaceName {
                return lhs.interfaceName < rhs.interfaceName
            }
            return lhs.address < rhs.address
        }?.address
    }

    private static func interfaceRank(_ name: String) -> Int {
        if name == "en0" { return 0 }
        if name.hasPrefix("en") { return 1 }
        return 2
    }
}

private final class ControllerPairingProxyResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value?

    func set(_ value: Value) {
        withControllerPairingProxyLock(lock) { self.value = value }
    }

    func get() -> Value? {
        withControllerPairingProxyLock(lock) { value }
    }
}

private func withControllerPairingProxyLock<T>(
    _ lock: NSLock,
    _ operation: () throws -> T
) rethrows -> T {
    lock.lock()
    defer { lock.unlock() }
    return try operation()
}
