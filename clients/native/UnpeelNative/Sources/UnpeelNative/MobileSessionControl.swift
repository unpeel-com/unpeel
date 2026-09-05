//
//  MobileSessionControl.swift
//  UnpeelNative
//
//  Controller-side artifact helpers that survive the Swift Host retirement:
//  the worker's `artifact.thumbnail` platform callback and the desktop
//  gallery read session artifacts through these. Terminal output, input,
//  resize, and every mobile route live in the Rust Host now.
//

import CryptoKit
import Foundation
import ImageIO
import UniformTypeIdentifiers
import UnpeelShared

enum MobileSessionControl {
    static func requiredSessionID(_ value: String?) throws -> String {
        let id = (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !id.isEmpty, !id.contains("/"), !id.contains("..") else {
            throw MobileRemoteError(400, "invalid session id")
        }
        return id
    }

    // MARK: - Browser MCP artifacts

    // Kind list + dir mapping live in the shared SessionArtifactStore
    // (SessionArtifacts.swift), which the desktop gallery reads too.

    /// Raw bytes per artifact chunk: sized so one chunk, base64'd and sealed,
    /// clears the relay's 512KB frame cap.
    private static let singleChunkBytes = 200 * 1024

    /// Avoid assembling pathological multi-gigabyte images merely because a
    /// Controller requested a gallery thumbnail. Larger sources fall back to
    /// the shared original-byte range response.
    private static let thumbnailSourceMaxBytes = 64 * 1024 * 1024

    /// ImageIO output is derived only from bytes already read through the
    /// Rust no-follow reader. NSCache is thread-safe and keeps repeated gallery
    /// polls from decoding the same source without creating another on-disk
    /// path that would need its own traversal/TOCTOU contract.
    private final class ThumbnailCache: @unchecked Sendable {
        private let values: NSCache<NSString, NSData> = {
            let cache = NSCache<NSString, NSData>()
            cache.countLimit = 64
            cache.totalCostLimit = 32 * 1024 * 1024
            return cache
        }()

        func value(for key: NSString) -> Data? {
            values.object(forKey: key) as Data?
        }

        func insert(_ value: Data, for key: NSString) {
            values.setObject(value as NSData, forKey: key, cost: value.count)
        }
    }

    private static let thumbnailCache = ThumbnailCache()

    private static func thumbnailData(source: Data, maxDim: Int) -> Data? {
        let dim = min(max(maxDim, 32), 1024)
        let digest = SHA256.hash(data: source)
            .map { String(format: "%02x", $0) }
            .joined()
        let cacheKey = "\(digest)-\(dim)" as NSString
        if let cached = thumbnailCache.value(for: cacheKey) {
            return cached
        }

        guard let imageSource = CGImageSourceCreateWithData(
            source as CFData,
            [kCGImageSourceShouldCache: false] as CFDictionary
        ) else { return nil }
        let options = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: dim,
        ] as CFDictionary
        guard let cgImage = CGImageSourceCreateThumbnailAtIndex(imageSource, 0, options) else {
            return nil
        }

        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            output,
            UTType.jpeg.identifier as CFString,
            1,
            nil
        ) else { return nil }
        CGImageDestinationAddImage(
            destination,
            cgImage,
            [kCGImageDestinationLossyCompressionQuality: 0.75] as CFDictionary
        )
        guard CGImageDestinationFinalize(destination) else { return nil }
        let encoded = output as Data
        thumbnailCache.insert(encoded, for: cacheKey)
        return encoded
    }

    /// A single path segment with no traversal — the same rule the desktop
    /// applies to session ids, applied to `kind`/`name` so a crafted request
    /// can't escape the artifacts dir.
    private static func safeArtifactSegment(_ value: String?) throws -> String {
        let segment = (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !segment.isEmpty,
              !segment.contains("/"),
              !segment.contains("\\"),
              !segment.contains("..")
        else {
            throw MobileRemoteError(400, "invalid artifact path")
        }
        return segment
    }

    /// GET /mobile/artifacts?session_id=… — the session's browser-MCP gallery
    /// (screenshots + downloads), metadata only, newest-first.
    static func browserArtifacts(query: [String: String]) throws -> RemoteBrowserArtifactList {
        let sessionID = try requiredSessionID(query["session_id"] ?? query["sessionID"])
        let artifacts = SessionArtifactStore.list(sessionID).map { artifact in
            RemoteBrowserArtifact(
                kind: artifact.kind,
                name: artifact.name,
                size: artifact.size,
                modifiedAtUnixMs: artifact.modifiedAt == .distantPast
                    ? 0 : MobilePairingStore.unixMs(artifact.modifiedAt)
            )
        }
        return RemoteBrowserArtifactList(
            sessionID: sessionID,
            artifacts: artifacts,
            capturedAtUnixMs: MobilePairingStore.unixMs(Date())
        )
    }

    /// GET /mobile/artifact?session_id=…&kind=…&name=…&offset=N&limit=M — one
    /// offset-addressed slice of an artifact's bytes. Ranged because a single
    /// screenshot far exceeds `RelayProtocol.maxFrameBytes` (512KB) once
    /// base64'd through the tunnel; the client reassembles across chunks.
    /// `max_dim=N` asks for a downscaled JPEG variant of an image artifact
    /// instead of the original bytes — the gallery grid path, so tiles don't
    /// pull multi-megabyte screenshots over the relay. The original file is
    /// never modified.
    static func browserArtifactChunk(query: [String: String]) throws -> RemoteBrowserArtifactChunk {
        guard let maxDim = Int(query["max_dim"] ?? ""), maxDim > 0 else {
            return try sharedOriginalArtifactChunk(query: query)
        }

        var metadataQuery = query
        metadataQuery.removeValue(forKey: "max_dim")
        metadataQuery["offset"] = "0"
        metadataQuery["limit"] = "1"
        let first = try sharedOriginalArtifactChunk(query: metadataQuery)

        // A file at or under one chunk is already one round trip. Non-images,
        // pathological source sizes, decode failures, and encode failures all
        // fall back through the same shared reader; Swift never reopens the
        // Controller-selected Host path.
        guard first.contentType.hasPrefix("image/"),
              first.totalSize > UInt64(singleChunkBytes),
              first.totalSize <= UInt64(thumbnailSourceMaxBytes),
              let source = try originalArtifactData(query: metadataQuery, first: first),
              let thumbnail = thumbnailData(source: source, maxDim: maxDim)
        else {
            return try sharedOriginalArtifactChunk(query: query)
        }

        let requestedLimit = query["limit"]
            .flatMap(Int.init)
            .flatMap { $0 >= 0 ? $0 : nil }
            ?? singleChunkBytes
        let limit = max(1, min(requestedLimit, singleChunkBytes))
        let start = min(
            query["offset"].flatMap(UInt64.init) ?? 0,
            UInt64(thumbnail.count)
        )
        let end = min(start + UInt64(limit), UInt64(thumbnail.count))
        let data = thumbnail.subdata(in: Int(start) ..< Int(end))
        return RemoteBrowserArtifactChunk(
            sessionID: first.sessionID,
            kind: first.kind,
            name: first.name,
            contentType: "image/jpeg",
            offset: start,
            nextOffset: end,
            totalSize: UInt64(thumbnail.count),
            dataBase64: data.base64EncodedString(),
            capturedAtUnixMs: MobilePairingStore.unixMs(Date())
        )
    }

    /// Route an original-byte request through the same Rust bridge used by
    /// authenticated HTTP/Link traffic. This helper is also the thumbnail
    /// adapter's only source of bytes; bridge failure is a hard failure, never
    /// permission to fall back to a lexical Swift path.
    private static func sharedOriginalArtifactChunk(
        query: [String: String]
    ) throws -> RemoteBrowserArtifactChunk {
        var originalQuery = query
        originalQuery.removeValue(forKey: "max_dim")
        let result = NativeControllerRouter.shared.route(
            requestID: nil,
            method: "GET",
            path: "/mobile/artifact",
            query: originalQuery,
            headers: [:],
            body: Data(),
            principal: NativeControllerPrincipal(
                deviceID: "native-thumbnail-adapter",
                name: "Native thumbnail adapter"
            ),
            routeContext: nil
        )
        switch result {
        case .handled(let response):
            guard response.status == 200 else {
                let object = response.body.data(using: .utf8).flatMap { data in
                    (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                }
                let message = object?["error"] as? String
                throw MobileRemoteError(response.status, message ?? "artifact read failed")
            }
            guard let data = response.body.data(using: .utf8) else {
                throw MobileRemoteError(500, "invalid artifact response")
            }
            return try JSONDecoder().decode(RemoteBrowserArtifactChunk.self, from: data)
        case .unhandled:
            throw MobileRemoteError(500, "shared artifact reader unavailable")
        case .bridgeUnavailable, .bridgeError:
            throw MobileRemoteError(500, "shared artifact reader failed")
        }
    }

    /// Reassemble one immutable source through bounded shared-reader chunks.
    /// `nil` means it changed or could not make progress; the caller then
    /// serves the requested original range through Rust.
    private static func originalArtifactData(
        query: [String: String],
        first: RemoteBrowserArtifactChunk
    ) throws -> Data? {
        guard let firstBytes = Data(base64Encoded: first.dataBase64),
              first.offset == 0,
              first.nextOffset == UInt64(firstBytes.count)
        else { return nil }
        var assembled = firstBytes
        let expectedTotal = first.totalSize
        for _ in 0 ..< 4096 {
            if UInt64(assembled.count) == expectedTotal { return assembled }
            if UInt64(assembled.count) > expectedTotal { return nil }
            var chunkQuery = query
            chunkQuery["offset"] = "\(assembled.count)"
            chunkQuery["limit"] = "\(singleChunkBytes)"
            let chunk = try sharedOriginalArtifactChunk(query: chunkQuery)
            guard chunk.sessionID == first.sessionID,
                  chunk.kind == first.kind,
                  chunk.name == first.name,
                  chunk.contentType == first.contentType,
                  chunk.totalSize == expectedTotal,
                  chunk.offset == UInt64(assembled.count),
                  let bytes = Data(base64Encoded: chunk.dataBase64),
                  !bytes.isEmpty,
                  chunk.nextOffset == chunk.offset + UInt64(bytes.count)
            else { return nil }
            assembled.append(bytes)
        }
        return nil
    }

    /// POST /mobile/artifact-delete?session_id=…&kind=…&name=… — remove one
    /// gallery artifact from disk (screenshot/download/upload). Idempotent: a
    /// missing file is a no-op success. Path segments are traversal-checked.
    static func deleteArtifact(query: [String: String]) throws -> [String: String] {
        let sessionID = try requiredSessionID(query["session_id"] ?? query["sessionID"])
        let kind = try safeArtifactSegment(query["kind"])
        guard SessionArtifactStore.kindDir(sessionID, kind: kind) != nil else {
            throw MobileRemoteError(404, "unknown artifact kind")
        }
        let name = try safeArtifactSegment(query["name"])
        try SessionArtifactStore.delete(sessionID, kind: kind, name: name)
        return ["ok": "true"]
    }

    private static func sessionDir(_ sessionID: String) -> URL {
        LaunchConfig.appSessionsDir.appendingPathComponent(sessionID)
    }
}
