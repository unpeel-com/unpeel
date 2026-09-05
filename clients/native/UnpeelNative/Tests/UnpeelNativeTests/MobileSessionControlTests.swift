import ImageIO
import UniformTypeIdentifiers
import XCTest
@testable import UnpeelNative

final class MobileSessionControlTests: XCTestCase {
    private func makeScreenshot(name: String, bytes: Data) throws -> (String, () -> Void) {
        let sessionID = "test-gallery-\(UUID().uuidString.prefix(8))"
        let dir = LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent("artifacts")
            .appendingPathComponent("browser")
            .appendingPathComponent("screenshots")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try bytes.write(to: dir.appendingPathComponent(name))
        let sessionRoot = LaunchConfig.appSessionsDir.appendingPathComponent(sessionID)
        return (sessionID, { try? FileManager.default.removeItem(at: sessionRoot) })
    }

    func testBrowserArtifactsListsScreenshots() throws {
        let (sessionID, cleanup) = try makeScreenshot(name: "page.png", bytes: Data([0x89, 0x50, 0x4E, 0x47]))
        defer { cleanup() }

        let list = try MobileSessionControl.browserArtifacts(query: ["session_id": sessionID])
        XCTAssertEqual(list.sessionID, sessionID)
        XCTAssertEqual(list.artifacts.count, 1)
        XCTAssertEqual(list.artifacts.first?.name, "page.png")
        XCTAssertEqual(list.artifacts.first?.kind, "screenshots")
        XCTAssertEqual(list.artifacts.first?.size, 4)
    }

    func testBrowserArtifactChunkReassemblesAcrossOffsets() throws {
        // 450KB forces multiple chunks at the 200KB relay-safe cap.
        let original = Data((0 ..< 450_000).map { UInt8($0 % 251) })
        let (sessionID, cleanup) = try makeScreenshot(name: "big.png", bytes: original)
        defer { cleanup() }

        var assembled = Data()
        var guardIterations = 0
        while assembled.count < original.count, guardIterations < 100 {
            guardIterations += 1
            let chunk = try MobileSessionControl.browserArtifactChunk(query: [
                "session_id": sessionID,
                "kind": "screenshots",
                "name": "big.png",
                "offset": "\(assembled.count)",
            ])
            XCTAssertEqual(chunk.totalSize, UInt64(original.count))
            XCTAssertLessThanOrEqual(chunk.dataBase64.count, 300 * 1024) // stays under a relay frame
            let bytes = try XCTUnwrap(Data(base64Encoded: chunk.dataBase64))
            XCTAssertFalse(bytes.isEmpty, "chunk must make progress")
            assembled.append(bytes)
        }
        XCTAssertEqual(assembled, original)
    }

    func testBrowserArtifactChunkRejectsTraversal() throws {
        let (sessionID, cleanup) = try makeScreenshot(name: "page.png", bytes: Data([0x00]))
        defer { cleanup() }

        XCTAssertThrowsError(try MobileSessionControl.browserArtifactChunk(query: [
            "session_id": sessionID,
            "kind": "screenshots",
            "name": "../../manifest.json",
        ]))
        XCTAssertThrowsError(try MobileSessionControl.browserArtifactChunk(query: [
            "session_id": sessionID,
            "kind": "secrets",
            "name": "page.png",
        ]))
    }

    /// A deterministic-noise PNG: incompressible, so it's guaranteed to be
    /// far larger than the single-chunk threshold that gates thumbnailing.
    private func makeNoisePNG(width: Int, height: Int) throws -> Data {
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        var seed: UInt64 = 0x9E37_79B9_7F4A_7C15
        for i in 0 ..< pixels.count {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            pixels[i] = UInt8(truncatingIfNeeded: seed >> 33)
        }
        let context = try XCTUnwrap(pixels.withUnsafeMutableBytes { buffer in
            CGContext(
                data: buffer.baseAddress,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
            )
        })
        let image = try XCTUnwrap(context.makeImage())
        let out = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(
            out, UTType.png.identifier as CFString, 1, nil
        ))
        CGImageDestinationAddImage(destination, image, nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return out as Data
    }

    private func thumbsDir(_ sessionID: String) -> URL {
        LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent("artifacts")
            .appendingPathComponent("thumbs")
    }

    func testBrowserArtifactChunkMaxDimServesDownscaledJpeg() throws {
        let png = try makeNoisePNG(width: 1600, height: 1600)
        XCTAssertGreaterThan(png.count, 200 * 1024, "fixture must exceed the thumbnail threshold")
        let (sessionID, cleanup) = try makeScreenshot(name: "big.png", bytes: png)
        defer { cleanup() }

        let query = [
            "session_id": sessionID,
            "kind": "screenshots",
            "name": "big.png",
            "max_dim": "256",
        ]
        let chunk = try MobileSessionControl.browserArtifactChunk(query: query)
        XCTAssertEqual(chunk.contentType, "image/jpeg")
        XCTAssertLessThan(chunk.totalSize, UInt64(png.count))
        let bytes = try XCTUnwrap(Data(base64Encoded: chunk.dataBase64))
        XCTAssertEqual(UInt64(bytes.count), chunk.totalSize, "thumbnail should fit one chunk")
        let source = try XCTUnwrap(CGImageSourceCreateWithData(bytes as CFData, nil))
        let decoded = try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
        XCTAssertLessThanOrEqual(max(decoded.width, decoded.height), 256)

        // Repeated requests are stable and the derived bytes never create a
        // second Controller-selected path on disk.
        let again = try MobileSessionControl.browserArtifactChunk(query: query)
        XCTAssertEqual(again.totalSize, chunk.totalSize)
        XCTAssertEqual(again.dataBase64, chunk.dataBase64)
        XCTAssertFalse(FileManager.default.fileExists(atPath: thumbsDir(sessionID).path))

        // The original bytes are untouched and still served without max_dim.
        let full = try MobileSessionControl.browserArtifactChunk(query: [
            "session_id": sessionID, "kind": "screenshots", "name": "big.png",
        ])
        XCTAssertEqual(full.contentType, "image/png")
        XCTAssertEqual(full.totalSize, UInt64(png.count))
    }

    func testBrowserArtifactChunkMaxDimSmallFileServesOriginal() throws {
        // At or under one chunk there's nothing to save — the original is one
        // round-trip already, so no thumbnail is generated.
        let png = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
        let (sessionID, cleanup) = try makeScreenshot(name: "small.png", bytes: png)
        defer { cleanup() }

        let chunk = try MobileSessionControl.browserArtifactChunk(query: [
            "session_id": sessionID,
            "kind": "screenshots",
            "name": "small.png",
            "max_dim": "256",
        ])
        XCTAssertEqual(chunk.contentType, "image/png")
        XCTAssertEqual(Data(base64Encoded: chunk.dataBase64), png)
        XCTAssertFalse(FileManager.default.fileExists(atPath: thumbsDir(sessionID).path))
    }

    func testDeleteArtifactReapsCachedThumbnails() throws {
        let (sessionID, cleanup) = try makeScreenshot(name: "big.png", bytes: Data([0x89]))
        defer { cleanup() }

        // Older builds cached thumbnails on disk. Keep deletion compatible so
        // an upgrade can reap those legacy siblings along with the original.
        let legacyThumbs = thumbsDir(sessionID)
        try FileManager.default.createDirectory(at: legacyThumbs, withIntermediateDirectories: true)
        try Data([0xFF, 0xD8, 0xFF]).write(
            to: legacyThumbs.appendingPathComponent("1-256-screenshots-big.png.jpg")
        )
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: thumbsDir(sessionID).path).count, 1)

        _ = try MobileSessionControl.deleteArtifact(query: [
            "session_id": sessionID, "kind": "screenshots", "name": "big.png",
        ])
        XCTAssertEqual(
            (try? FileManager.default.contentsOfDirectory(atPath: thumbsDir(sessionID).path))?.count ?? 0,
            0
        )
    }
}
