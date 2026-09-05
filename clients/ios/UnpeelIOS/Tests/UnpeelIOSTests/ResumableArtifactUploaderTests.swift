import CryptoKit
import XCTest
import UnpeelShared
#if canImport(UIKit)
import UIKit
#endif
#if canImport(UnpeelIOS)
@testable import UnpeelIOS
#elseif canImport(Unpeel)
@testable import Unpeel
#endif

final class ResumableArtifactUploaderTests: XCTestCase {
    func testGalleryNormalizerPreservesLegacyGIFPayload() throws {
        let gif = try XCTUnwrap(Data(base64Encoded: "R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=="))

        let payload = try XCTUnwrap(RemoteGalleryAttachmentNormalizer.normalize(
            data: gif,
            contentType: "image/gif",
            forResumableUpload: false
        ))

        XCTAssertEqual(payload.data, gif)
        XCTAssertEqual(payload.contentType, "image/gif")
    }

    func testGalleryNormalizerTranscodesGIFForResumableHost() throws {
        let gif = try XCTUnwrap(Data(base64Encoded: "R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=="))

        let payload = try XCTUnwrap(RemoteGalleryAttachmentNormalizer.normalize(
            data: gif,
            contentType: "image/gif",
            forResumableUpload: true
        ))

        XCTAssertNotEqual(payload.data, gif)
        XCTAssertTrue(["image/jpeg", "image/png"].contains(payload.contentType))
        XCTAssertNotNil(UIImage(data: payload.data))
        XCTAssertLessThanOrEqual(payload.data.count, ResumableArtifactUploader.maximumSize)
    }

    func testGalleryNormalizerLeavesSupportedResumablePayloadUntouched() throws {
        let png = Data([0x89, 0x50, 0x4E, 0x47, 1, 2, 3])

        let payload = try XCTUnwrap(RemoteGalleryAttachmentNormalizer.normalize(
            data: png,
            contentType: "image/png",
            forResumableUpload: true
        ))

        XCTAssertEqual(payload.data, png)
        XCTAssertEqual(payload.contentType, "image/png")
    }

    func testChunksAtRelaySafeBoundaryAndUsesWholeFileDigest() async throws {
        let bytes = Data((0 ..< ResumableArtifactUploader.chunkSize + 17).map {
            UInt8(truncatingIfNeeded: $0)
        })
        let recorder = UploadRecorder()

        let path = try await ResumableArtifactUploader.upload(
            sessionID: "session-1",
            data: bytes,
            contentType: "image/png",
            uploadID: "upload-fixed",
            sendChunk: { try await recorder.send($0) }
        )

        let chunks = await recorder.recordedChunks()
        XCTAssertEqual(path, "/host/artifacts/uploads/upload-fixed.png")
        XCTAssertEqual(chunks.map(\.offset), [0, ResumableArtifactUploader.chunkSize])
        XCTAssertEqual(chunks.map { $0.body.count }, [ResumableArtifactUploader.chunkSize, 17])
        XCTAssertEqual(chunks.map(\.uploadID), ["upload-fixed", "upload-fixed"])
        XCTAssertEqual(chunks.map(\.totalSize), [bytes.count, bytes.count])
        XCTAssertEqual(chunks.map(\.contentType), ["image/png", "image/png"])
        XCTAssertEqual(chunks.map(\.sha256), [sha256(bytes), sha256(bytes)])
        XCTAssertEqual(chunks.reduce(into: Data()) { $0.append($1.body) }, bytes)
    }

    func testRetriesUncertainFailureWithIdenticalChunk() async throws {
        let bytes = Data(repeating: 0xA5, count: 100)
        let recorder = UploadRecorder(failFirstTransportAttempt: true)

        let path = try await ResumableArtifactUploader.upload(
            sessionID: "session-1",
            data: bytes,
            contentType: "image/jpeg",
            uploadID: "upload-retry",
            sendChunk: { try await recorder.send($0) }
        )

        let chunks = await recorder.recordedChunks()
        XCTAssertEqual(path, "/host/artifacts/uploads/upload-retry.jpg")
        XCTAssertEqual(chunks.count, 2)
        XCTAssertEqual(chunks[0], chunks[1])
    }

    func testDoesNotRetryHTTPFailure() async throws {
        let attempts = AttemptCounter()

        do {
            _ = try await ResumableArtifactUploader.upload(
                sessionID: "session-1",
                data: Data([1, 2, 3]),
                contentType: "image/jpeg",
                uploadID: "upload-http-error",
                sendChunk: { _ in
                    await attempts.increment()
                    throw RemoteMacClientError(statusCode: 409, serverMessage: "offset conflict")
                }
            )
            XCTFail("expected HTTP error")
        } catch let error as RemoteMacClientError {
            XCTAssertEqual(error.statusCode, 409)
        }

        let attemptCount = await attempts.value()
        XCTAssertEqual(attemptCount, 1)
    }

    func testRejectsInvalidLocalInputBeforeSending() async throws {
        let attempts = AttemptCounter()
        let send: ResumableArtifactUploader.SendChunk = { _ in
            await attempts.increment()
            throw URLError(.unknown)
        }

        await assertUploadError(.emptyPayload) {
            try await ResumableArtifactUploader.upload(
                sessionID: "session-1",
                data: Data(),
                contentType: "image/png",
                uploadID: "empty",
                sendChunk: send
            )
        }
        await assertUploadError(.payloadTooLarge) {
            try await ResumableArtifactUploader.upload(
                sessionID: "session-1",
                data: Data(repeating: 0, count: ResumableArtifactUploader.maximumSize + 1),
                contentType: "image/png",
                uploadID: "large",
                sendChunk: send
            )
        }
        await assertUploadError(.unsupportedContentType) {
            try await ResumableArtifactUploader.upload(
                sessionID: "session-1",
                data: Data([1]),
                contentType: "image/gif",
                uploadID: "gif",
                sendChunk: send
            )
        }

        let attemptCount = await attempts.value()
        XCTAssertEqual(attemptCount, 0)
    }

    private func sha256(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private func assertUploadError(
        _ expected: RemoteArtifactUploadError,
        operation: () async throws -> String
    ) async {
        do {
            _ = try await operation()
            XCTFail("expected \(expected)")
        } catch let error as RemoteArtifactUploadError {
            XCTAssertEqual(error, expected)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }
}

private actor UploadRecorder {
    private var chunks: [ResumableArtifactUploader.Chunk] = []
    private var shouldFailTransport: Bool

    init(failFirstTransportAttempt: Bool = false) {
        shouldFailTransport = failFirstTransportAttempt
    }

    func send(
        _ chunk: ResumableArtifactUploader.Chunk
    ) throws -> RemoteArtifactUploadProgress {
        chunks.append(chunk)
        if shouldFailTransport {
            shouldFailTransport = false
            throw URLError(.timedOut)
        }
        let nextOffset = chunk.offset + chunk.body.count
        let complete = nextOffset == chunk.totalSize
        let suffix = chunk.contentType == "image/png" ? "png" : "jpg"
        return RemoteArtifactUploadProgress(
            uploadID: chunk.uploadID,
            nextOffset: UInt64(nextOffset),
            complete: complete,
            path: complete ? "/host/artifacts/uploads/\(chunk.uploadID).\(suffix)" : nil
        )
    }

    func recordedChunks() -> [ResumableArtifactUploader.Chunk] { chunks }
}

private actor AttemptCounter {
    private var attempts = 0

    func increment() { attempts += 1 }
    func value() -> Int { attempts }
}
