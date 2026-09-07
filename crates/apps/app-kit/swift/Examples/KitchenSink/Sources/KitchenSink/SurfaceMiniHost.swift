import Foundation
import SwiftUI
import UnpeelAppKitUI

struct SurfaceWebEndpoint: Equatable, Sendable {
    let host: String
    let port: UInt16
    let token: String

    var baseURL: String { "http://\(host):\(port)" }
    var moduleURL: String { "\(baseURL)/surface.js?token=\(token)" }
    var wasmURL: String { "\(baseURL)/surface.wasm?token=\(token)" }
}

#if UNPEEL_SURFACE_KIT
import AppKit
import Darwin
import UnpeelSurfaceKit

private enum SurfaceMiniHostError: LocalizedError {
    case webAssetsMissing
    case socket(String)
    case socketPathTooLong

    var errorDescription: String? {
        switch self {
        case .webAssetsMissing:
            "Could not find unpeel-surface/web/pkg. Build its web package or set UNPEEL_SURFACE_WEB_PKG."
        case let .socket(operation):
            "Surface mini-host could not \(operation): \(String(cString: strerror(errno)))"
        case .socketPathTooLong:
            "Surface mini-host Unix socket path is too long"
        }
    }
}

private struct SurfaceWebAssets: Sendable {
    let module: URL
    let wasm: URL
}

/// A deliberately small local Host adapter for the Kitchen Sink.
///
/// It accepts one app-owned USRF producer over a private Unix socket and fans
/// the retained resource/scene packets out to local Metal and WebGPU
/// presenters. Packets are never decoded into pixels here. The loopback HTTP
/// side exists only because unpeel-surface's browser presenter consumes a
/// streamed USRF route; production Unpeel uses its authenticated Host routes.
final class SurfaceMiniBroker: @unchecked Sendable {
    static let logicalWidth: UInt32 = 960
    static let logicalHeight: UInt32 = 600

    let socketPath: String
    let webEndpoint: SurfaceWebEndpoint?

    private static let headerBytes = 20
    private static let maximumPacketBytes = 256 * 1024 * 1024 + headerBytes
    private static let resourceKind: UInt16 = 3
    private static let sceneKind: UInt16 = 4
    private static let eventKind: UInt16 = 5
    private static let resizeKind: UInt16 = 6

    private let assets: SurfaceWebAssets
    private let stateLock = NSLock()
    private let producerWriteLock = NSLock()
    private var unixListenFD: Int32
    private var httpListenFD: Int32
    private var producerFD: Int32 = -1
    private var stopped = false
    private var resources: [UInt64: Data] = [:]
    private var latestScene: Data?
    private var nativeSubscribers: [UUID: @MainActor @Sendable (Data, Bool) -> Void] = [:]
    private var webStreams: [UUID: SurfaceHTTPStream] = [:]

    init(sessionDirectory: String) throws {
        guard let assets = Self.locateWebAssets() else {
            throw SurfaceMiniHostError.webAssetsMissing
        }
        self.assets = assets
        socketPath = URL(fileURLWithPath: sessionDirectory, isDirectory: true)
            .appendingPathComponent("surface.sock")
            .path
        unixListenFD = try Self.makeUnixListener(path: socketPath)
        do {
            let (fd, port) = try Self.makeHTTPlistener()
            httpListenFD = fd
            webEndpoint = SurfaceWebEndpoint(
                host: "127.0.0.1",
                port: port,
                token: Self.randomToken()
            )
        } catch {
            Darwin.close(unixListenFD)
            unlink(socketPath)
            throw error
        }
        startAcceptLoops()
    }

    deinit {
        stop()
    }

    func stop() {
        let descriptors: (Int32, Int32, Int32, [SurfaceHTTPStream]) = stateLock.withLock {
            guard !stopped else { return (-1, -1, -1, []) }
            stopped = true
            let values = (unixListenFD, httpListenFD, producerFD, Array(webStreams.values))
            unixListenFD = -1
            httpListenFD = -1
            producerFD = -1
            webStreams.removeAll()
            nativeSubscribers.removeAll()
            return values
        }
        for descriptor in [descriptors.0, descriptors.1, descriptors.2] where descriptor >= 0 {
            Darwin.shutdown(descriptor, SHUT_RDWR)
            Darwin.close(descriptor)
        }
        descriptors.3.forEach { $0.close() }
        unlink(socketPath)
    }

    @MainActor
    func subscribe(
        _ receive: @escaping @MainActor @Sendable (Data, Bool) -> Void
    ) -> UUID {
        let id = UUID()
        let replay: [Data] = stateLock.withLock {
            let packets = resources.keys.sorted().compactMap { resources[$0] }
                + (latestScene.map { [$0] } ?? [])
            nativeSubscribers[id] = receive
            return packets
        }
        for (index, packet) in replay.enumerated() {
            receive(packet, index == 0)
        }
        return id
    }

    @MainActor
    func unsubscribe(_ id: UUID) {
        _ = stateLock.withLock { nativeSubscribers.removeValue(forKey: id) }
    }

    func sendToProducer(_ packet: Data) {
        guard let header = Self.packetHeader(packet),
              header.kind == Self.eventKind
                || header.kind == Self.resizeKind && Self.isCanonicalResize(packet)
        else { return }
        producerWriteLock.withLock {
            let fd = stateLock.withLock { producerFD }
            guard fd >= 0 else { return }
            _ = Self.writeAll(packet, to: fd)
        }
    }

    private func startAcceptLoops() {
        let producerListener = unixListenFD
        let producerThread = Thread { [weak self] in
            self?.acceptProducers(listener: producerListener)
        }
        producerThread.name = "app-kit.surface-producer"
        producerThread.start()

        let webListener = httpListenFD
        let webThread = Thread { [weak self] in
            self?.acceptHTTP(listener: webListener)
        }
        webThread.name = "app-kit.surface-web"
        webThread.start()
    }

    private func acceptProducers(listener: Int32) {
        while true {
            let client = accept(listener, nil, nil)
            guard client >= 0 else {
                if stateLock.withLock({ stopped }) || errno == EBADF { return }
                continue
            }
            Self.prepareAcceptedSocket(client)
            let oldProducer: Int32 = stateLock.withLock {
                let old = producerFD
                producerFD = client
                resources.removeAll(keepingCapacity: true)
                latestScene = nil
                return old
            }
            if oldProducer >= 0 {
                Darwin.shutdown(oldProducer, SHUT_RDWR)
                Darwin.close(oldProducer)
            }
            resetNativePresenters()
            let thread = Thread { [weak self] in
                self?.readProducer(client)
            }
            thread.name = "app-kit.surface-usrf"
            thread.start()
        }
    }

    private func readProducer(_ fd: Int32) {
        var pending = Data()
        var bytes = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let count = Darwin.read(fd, &bytes, bytes.count)
            guard count > 0 else { break }
            pending.append(contentsOf: bytes.prefix(count))
            while let header = Self.packetHeaderPrefix(pending) {
                guard header.totalBytes <= Self.maximumPacketBytes else { return }
                guard pending.count >= header.totalBytes else { break }
                let packet = Data(pending.prefix(header.totalBytes))
                pending.removeFirst(header.totalBytes)
                receiveProducerPacket(packet, header: header)
            }
            if pending.count > Self.maximumPacketBytes { break }
        }
        let shouldClose = stateLock.withLock { () -> Bool in
            guard producerFD == fd else { return false }
            producerFD = -1
            return true
        }
        if shouldClose {
            Darwin.close(fd)
        }
    }

    private func receiveProducerPacket(_ packet: Data, header: SurfacePacketHeader) {
        guard header.kind == Self.resourceKind || header.kind == Self.sceneKind else { return }
        let delivery: (
            [@MainActor @Sendable (Data, Bool) -> Void],
            [SurfaceHTTPStream]
        ) = stateLock.withLock {
            if header.kind == Self.resourceKind, packet.count >= Self.headerBytes + 8 {
                resources[Self.littleEndianUInt64(packet, at: Self.headerBytes)] = packet
            } else if header.kind == Self.sceneKind {
                latestScene = packet
            }
            return (Array(nativeSubscribers.values), Array(webStreams.values))
        }
        for callback in delivery.0 {
            Task { @MainActor in callback(packet, false) }
        }
        delivery.1.forEach { $0.send(packet) }
    }

    private func resetNativePresenters() {
        let callbacks = stateLock.withLock { Array(nativeSubscribers.values) }
        for callback in callbacks {
            Task { @MainActor in callback(Data(), true) }
        }
    }

    private func acceptHTTP(listener: Int32) {
        while true {
            let client = accept(listener, nil, nil)
            guard client >= 0 else {
                if stateLock.withLock({ stopped }) || errno == EBADF { return }
                continue
            }
            Self.prepareAcceptedSocket(client)
            let thread = Thread { [weak self] in
                self?.handleHTTP(client)
            }
            thread.name = "app-kit.surface-http"
            thread.start()
        }
    }

    private func handleHTTP(_ fd: Int32) {
        var ownsDescriptor = true
        defer {
            if ownsDescriptor { Darwin.close(fd) }
        }
        guard let request = Self.readHTTPRequest(fd), authorized(request.target) else {
            Self.respond(fd, status: 401, reason: "Unauthorized", body: Data())
            return
        }
        let path = request.target.split(separator: "?", maxSplits: 1).first.map(String.init) ?? ""
        if request.method == "OPTIONS" {
            Self.respond(fd, status: 204, reason: "No Content", body: Data(), preflight: true)
        } else if request.method == "GET", path == "/surface.js" {
            serve(assets.module, contentType: "text/javascript; charset=utf-8", to: fd)
        } else if request.method == "GET", path == "/surface.wasm" {
            serve(assets.wasm, contentType: "application/wasm", to: fd)
        } else if request.method == "GET", path == "/stream" {
            guard Self.writeString(
                "HTTP/1.1 200 OK\r\n"
                    + "Content-Type: application/octet-stream\r\n"
                    + "Cache-Control: no-store\r\n"
                    + "Access-Control-Allow-Origin: *\r\n"
                    + "Transfer-Encoding: chunked\r\n"
                    + "Connection: keep-alive\r\n\r\n",
                to: fd
            ) else { return }
            ownsDescriptor = false
            registerWebStream(fd)
        } else if request.method == "POST", path == "/input",
                  let header = Self.packetHeader(request.body),
                  header.kind == Self.eventKind
                    || header.kind == Self.resizeKind && Self.isCanonicalResize(request.body) {
            sendToProducer(request.body)
            Self.respond(fd, status: 204, reason: "No Content", body: Data())
        } else {
            Self.respond(fd, status: 404, reason: "Not Found", body: Data())
        }
    }

    private func authorized(_ target: String) -> Bool {
        guard let token = webEndpoint?.token else { return false }
        let query = target.split(separator: "?", maxSplits: 1).dropFirst().first ?? ""
        return query.split(separator: "&").contains { item in
            let pair = item.split(separator: "=", maxSplits: 1)
            return pair.count == 2 && pair[0] == "token" && pair[1] == Substring(token)
        }
    }

    private func serve(_ url: URL, contentType: String, to fd: Int32) {
        guard let bytes = try? Data(contentsOf: url, options: .mappedIfSafe) else {
            Self.respond(fd, status: 500, reason: "Internal Server Error", body: Data())
            return
        }
        Self.respond(fd, status: 200, reason: "OK", body: bytes, contentType: contentType)
    }

    private func registerWebStream(_ fd: Int32) {
        let id = UUID()
        let stream = SurfaceHTTPStream(id: id, descriptor: fd) { [weak self] id in
            self?.removeWebStream(id)
        }
        let primed = stateLock.withLock { () -> Bool in
            let replay = resources.keys.sorted().compactMap { resources[$0] }
                + (latestScene.map { [$0] } ?? [])
            guard stream.prime(with: replay) else { return false }
            webStreams[id] = stream
            return true
        }
        if primed {
            stream.start()
        } else {
            stream.close()
        }
    }

    private func removeWebStream(_ id: UUID) {
        _ = stateLock.withLock { webStreams.removeValue(forKey: id) }
    }

    private static func locateWebAssets() -> SurfaceWebAssets? {
        var candidates: [URL] = []
        if let override = ProcessInfo.processInfo.environment["UNPEEL_SURFACE_WEB_PKG"],
           !override.isEmpty {
            candidates.append(URL(fileURLWithPath: override, isDirectory: true))
        }
        var directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        for _ in 0..<12 {
            let manifest = directory.appendingPathComponent("Cargo.toml")
            if let contents = try? String(contentsOf: manifest, encoding: .utf8),
               contents.contains("name = \"unpeel-app-kit\"") {
                candidates.append(
                    directory.deletingLastPathComponent()
                        .appendingPathComponent("unpeel-surface/web/pkg", isDirectory: true)
                )
                break
            }
            let parent = directory.deletingLastPathComponent()
            if parent == directory { break }
            directory = parent
        }
        for candidate in candidates {
            let module = candidate.appendingPathComponent("unpeel_surface_web.js")
            let wasm = candidate.appendingPathComponent("unpeel_surface_web_bg.wasm")
            if FileManager.default.fileExists(atPath: module.path),
               FileManager.default.fileExists(atPath: wasm.path) {
                return SurfaceWebAssets(module: module, wasm: wasm)
            }
        }
        return nil
    }

    private static func makeUnixListener(path: String) throws -> Int32 {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw SurfaceMiniHostError.socket("create its Unix socket") }
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
        unlink(path)
        var address = sockaddr_un()
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        address.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard pathBytes.count < capacity else {
            Darwin.close(fd)
            throw SurfaceMiniHostError.socketPathTooLong
        }
        withUnsafeMutableBytes(of: &address.sun_path) { destination in
            destination.initializeMemory(as: UInt8.self, repeating: 0)
            pathBytes.withUnsafeBytes { source in
                destination.copyBytes(from: source)
            }
        }
        let bound = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0, chmod(path, 0o600) == 0, listen(fd, 8) == 0 else {
            Darwin.close(fd)
            unlink(path)
            throw SurfaceMiniHostError.socket("bind its Unix socket")
        }
        return fd
    }

    private static func makeHTTPlistener() throws -> (Int32, UInt16) {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw SurfaceMiniHostError.socket("create its HTTP socket") }
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = 0
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, listen(fd, 16) == 0 else {
            Darwin.close(fd)
            throw SurfaceMiniHostError.socket("bind its HTTP socket")
        }
        var assigned = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let readName = withUnsafeMutablePointer(to: &assigned) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(fd, $0, &length)
            }
        }
        guard readName == 0 else {
            Darwin.close(fd)
            throw SurfaceMiniHostError.socket("read its HTTP port")
        }
        return (fd, UInt16(bigEndian: assigned.sin_port))
    }

    private static func prepareAcceptedSocket(_ fd: Int32) {
        _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
        var noSignal: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    private struct HTTPRequest {
        let method: String
        let target: String
        let body: Data
    }

    private static func readHTTPRequest(_ fd: Int32) -> HTTPRequest? {
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 16 * 1024)
        var headerRange: Range<Data.Index>?
        var contentLength = 0
        while buffer.count <= 1024 * 1024 + 64 * 1024 {
            if headerRange == nil, let range = buffer.range(of: Data("\r\n\r\n".utf8)) {
                headerRange = range
                guard let header = String(
                    data: buffer[buffer.startIndex..<range.lowerBound],
                    encoding: .utf8
                ) else { return nil }
                for line in header.components(separatedBy: "\r\n").dropFirst() {
                    if line.lowercased().hasPrefix("content-length:") {
                        guard let value = Int(line.dropFirst("content-length:".count)
                            .trimmingCharacters(in: .whitespaces)),
                              value >= 0, value <= 1024 * 1024 else { return nil }
                        contentLength = value
                    }
                }
            }
            if let range = headerRange {
                let bodyStart = range.upperBound
                if buffer.count - bodyStart >= contentLength {
                    guard let header = String(
                        data: buffer[buffer.startIndex..<range.lowerBound],
                        encoding: .utf8
                    ), let requestLine = header.components(separatedBy: "\r\n").first else {
                        return nil
                    }
                    let parts = requestLine.split(separator: " ")
                    guard parts.count == 3 else { return nil }
                    let bodyEnd = bodyStart + contentLength
                    return HTTPRequest(
                        method: String(parts[0]),
                        target: String(parts[1]),
                        body: Data(buffer[bodyStart..<bodyEnd])
                    )
                }
            }
            let count = Darwin.read(fd, &chunk, chunk.count)
            guard count > 0 else { return nil }
            buffer.append(contentsOf: chunk.prefix(count))
        }
        return nil
    }

    private static func respond(
        _ fd: Int32,
        status: Int,
        reason: String,
        body: Data,
        contentType: String = "application/octet-stream",
        preflight: Bool = false
    ) {
        var header = "HTTP/1.1 \(status) \(reason)\r\n"
            + "Content-Length: \(body.count)\r\n"
            + "Content-Type: \(contentType)\r\n"
            + "Access-Control-Allow-Origin: *\r\n"
            + "Cache-Control: no-store\r\n"
        if preflight {
            header += "Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n"
                + "Access-Control-Allow-Headers: Content-Type\r\n"
        }
        header += "Connection: close\r\n\r\n"
        guard writeString(header, to: fd) else { return }
        _ = writeAll(body, to: fd)
    }

    private static func writeString(_ value: String, to fd: Int32) -> Bool {
        writeAll(Data(value.utf8), to: fd)
    }

    fileprivate static func writeAll(_ data: Data, to fd: Int32) -> Bool {
        data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { return true }
            var written = 0
            while written < bytes.count {
                let result = Darwin.write(fd, base.advanced(by: written), bytes.count - written)
                if result > 0 {
                    written += result
                } else if result < 0, errno == EINTR {
                    continue
                } else {
                    return false
                }
            }
            return true
        }
    }

    private struct SurfacePacketHeader {
        let kind: UInt16
        let totalBytes: Int
    }

    private static func packetHeaderPrefix(_ data: Data) -> SurfacePacketHeader? {
        guard data.count >= headerBytes else { return nil }
        guard byte(data, at: 0) == 0x55, byte(data, at: 1) == 0x53,
              byte(data, at: 2) == 0x52, byte(data, at: 3) == 0x46,
              littleEndianUInt16(data, at: 4) == 1 else { return nil }
        let payload = Int(littleEndianUInt32(data, at: 8))
        return SurfacePacketHeader(
            kind: littleEndianUInt16(data, at: 6),
            totalBytes: headerBytes + payload
        )
    }

    private static func packetHeader(_ data: Data) -> SurfacePacketHeader? {
        guard let header = packetHeaderPrefix(data), header.totalBytes == data.count,
              header.totalBytes <= maximumPacketBytes else { return nil }
        return header
    }

    private static func isCanonicalResize(_ packet: Data) -> Bool {
        guard packet.count >= headerBytes + 8 else { return false }
        return littleEndianUInt32(packet, at: headerBytes) == logicalWidth
            && littleEndianUInt32(packet, at: headerBytes + 4) == logicalHeight
    }

    private static func littleEndianUInt16(_ data: Data, at offset: Int) -> UInt16 {
        UInt16(byte(data, at: offset)) | UInt16(byte(data, at: offset + 1)) << 8
    }

    private static func littleEndianUInt32(_ data: Data, at offset: Int) -> UInt32 {
        UInt32(byte(data, at: offset))
            | UInt32(byte(data, at: offset + 1)) << 8
            | UInt32(byte(data, at: offset + 2)) << 16
            | UInt32(byte(data, at: offset + 3)) << 24
    }

    private static func littleEndianUInt64(_ data: Data, at offset: Int) -> UInt64 {
        UInt64(littleEndianUInt32(data, at: offset))
            | UInt64(littleEndianUInt32(data, at: offset + 4)) << 32
    }

    private static func byte(_ data: Data, at offset: Int) -> UInt8 {
        data[data.index(data.startIndex, offsetBy: offset)]
    }

    private static func randomToken() -> String {
        Data((0..<24).map { _ in UInt8.random(in: .min ... .max) })
            .base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

private final class SurfaceHTTPStream: @unchecked Sendable {
    private struct PendingPacket {
        let kind: UInt16
        let data: Data
    }

    let id: UUID
    private let descriptor: Int32
    private let queue: DispatchQueue
    private let stateLock = NSLock()
    private var closed = false
    private var draining = false
    private var pending: [PendingPacket] = []
    private let onClose: @Sendable (UUID) -> Void
    private static let maximumPendingPackets = 512

    init(id: UUID, descriptor: Int32, onClose: @escaping @Sendable (UUID) -> Void) {
        self.id = id
        self.descriptor = descriptor
        self.onClose = onClose
        queue = DispatchQueue(label: "app-kit.surface-stream.\(id.uuidString)")
    }

    func send(_ packet: Data) {
        let kind = Self.packetKind(packet) ?? 0
        let result: (startDrain: Bool, overflowed: Bool) = stateLock.withLock {
            guard !closed else { return (false, false) }
            guard enqueue(PendingPacket(kind: kind, data: packet)) else {
                closed = true
                pending.removeAll(keepingCapacity: false)
                return (false, true)
            }
            guard !draining else { return (false, false) }
            draining = true
            return (true, false)
        }
        if result.overflowed {
            closeDescriptor()
            onClose(id)
        } else if result.startDrain {
            queue.async { [weak self] in self?.drain() }
        }
    }

    /// Seeds retained resources and the latest scene before the stream is
    /// visible to producer delivery. This keeps replay-before-live ordering.
    func prime(with packets: [Data]) -> Bool {
        stateLock.withLock {
            guard !closed, !draining, pending.isEmpty else { return false }
            for packet in packets {
                let kind = Self.packetKind(packet) ?? 0
                guard enqueue(PendingPacket(kind: kind, data: packet)) else {
                    pending.removeAll(keepingCapacity: false)
                    return false
                }
            }
            return true
        }
    }

    func start() {
        let shouldStart = stateLock.withLock { () -> Bool in
            guard !closed, !draining, !pending.isEmpty else { return false }
            draining = true
            return true
        }
        if shouldStart {
            queue.async { [weak self] in self?.drain() }
        }
    }

    func close() {
        let shouldClose = stateLock.withLock { () -> Bool in
            guard !closed else { return false }
            closed = true
            pending.removeAll(keepingCapacity: false)
            return true
        }
        if shouldClose { closeDescriptor() }
    }

    private func drain() {
        while true {
            let next: PendingPacket? = stateLock.withLock {
                guard !closed else {
                    draining = false
                    return nil
                }
                guard !pending.isEmpty else {
                    draining = false
                    return nil
                }
                return pending.removeFirst()
            }
            guard let next else { return }
            let prefix = Data(String(next.data.count, radix: 16).utf8) + Data("\r\n".utf8)
            let suffix = Data("\r\n".utf8)
            guard SurfaceMiniBroker.writeAll(prefix, to: descriptor),
                  SurfaceMiniBroker.writeAll(next.data, to: descriptor),
                  SurfaceMiniBroker.writeAll(suffix, to: descriptor) else {
                close()
                onClose(id)
                return
            }
        }
    }

    private func enqueue(_ packet: PendingPacket) -> Bool {
        // Retained scenes supersede any scene this client has not begun
        // writing yet. Resources and control packets remain ordered.
        if packet.kind == 4, let index = pending.lastIndex(where: { $0.kind == 4 }) {
            pending[index] = packet
            return true
        }
        guard pending.count < Self.maximumPendingPackets else { return false }
        pending.append(packet)
        return true
    }

    private func closeDescriptor() {
        Darwin.shutdown(descriptor, SHUT_RDWR)
        Darwin.close(descriptor)
    }

    private static func packetKind(_ packet: Data) -> UInt16? {
        guard packet.count >= 8 else { return nil }
        let start = packet.startIndex
        let low = packet[packet.index(start, offsetBy: 6)]
        let high = packet[packet.index(start, offsetBy: 7)]
        return UInt16(low) | UInt16(high) << 8
    }
}

/// SwiftUI adapter around unpeel-surface's transport-free AppKit presenter.
/// The component wrapper owns layout; Surface owns USRF decoding and Metal.
struct KitchenSinkSurfacePresenter: NSViewRepresentable {
    let broker: SurfaceMiniBroker
    let interactive: Bool
    let background: SurfaceBackground

    final class Coordinator {
        let broker: SurfaceMiniBroker
        var subscription: UUID?

        init(broker: SurfaceMiniBroker) {
            self.broker = broker
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(broker: broker)
    }

    func makeNSView(context: Context) -> RemoteSurfacePresenterView {
        let presenter = RemoteSurfacePresenterView(
            fixedViewport: CGSize(
                width: CGFloat(SurfaceMiniBroker.logicalWidth),
                height: CGFloat(SurfaceMiniBroker.logicalHeight)
            )
        )
        configure(presenter)
        context.coordinator.subscription = broker.subscribe { [weak presenter] packet, reset in
            do {
                _ = try presenter?.receive(packet, reset: reset)
            } catch {
                NSLog("Kitchen Sink Surface presenter rejected USRF: %@", String(describing: error))
            }
        }
        return presenter
    }

    func updateNSView(_ presenter: RemoteSurfacePresenterView, context _: Context) {
        configure(presenter)
    }

    static func dismantleNSView(
        _ presenter: RemoteSurfacePresenterView,
        coordinator: Coordinator
    ) {
        if let subscription = coordinator.subscription {
            coordinator.broker.unsubscribe(subscription)
        }
        presenter.clear()
    }

    private func configure(_ presenter: RemoteSurfacePresenterView) {
        presenter.compositingBackgroundColor = nativeColor(background)
        presenter.onInput = interactive ? { [weak broker] packet in
            broker?.sendToProducer(packet.data)
        } : nil
        presenter.onError = { error in
            NSLog("Kitchen Sink Surface presenter error: %@", String(describing: error))
        }
    }

    private func nativeColor(_ background: SurfaceBackground) -> NSColor {
        switch background {
        case .transparent:
            return .clear
        case let .solid(value):
            guard value.hasPrefix("#"), value.count == 7 || value.count == 9,
                  let packed = UInt64(value.dropFirst(), radix: 16)
            else { return .clear }
            let hasAlpha = value.count == 9
            let redShift = hasAlpha ? 24 : 16
            let greenShift = hasAlpha ? 16 : 8
            let blueShift = hasAlpha ? 8 : 0
            return NSColor(
                srgbRed: CGFloat((packed >> redShift) & 0xff) / 255,
                green: CGFloat((packed >> greenShift) & 0xff) / 255,
                blue: CGFloat((packed >> blueShift) & 0xff) / 255,
                alpha: hasAlpha ? CGFloat(packed & 0xff) / 255 : 1
            )
        }
    }
}

#else

final class SurfaceMiniBroker {
    static let logicalWidth: UInt32 = 960
    static let logicalHeight: UInt32 = 600
    let socketPath = ""
    let webEndpoint: SurfaceWebEndpoint? = nil

    init(sessionDirectory _: String) throws {
        throw NSError(
            domain: "UnpeelAppKitKitchenSink.Surface",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "UnpeelSurfaceKit is not available"]
        )
    }

    func stop() {}
}

struct KitchenSinkSurfacePresenter: View {
    let broker: SurfaceMiniBroker
    let interactive: Bool
    let background: SurfaceBackground

    var body: some View { EmptyView() }
}

#endif
