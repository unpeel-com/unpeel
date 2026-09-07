import AppKit
import CryptoKit
import SwiftUI

/// Authorized blob loader supplied by the existing native Host.
///
/// The loader should use the current App Session route and Controller grants;
/// Media never defines another server or embeds large bytes in snapshots.
public typealias MediaBlobLoader = @Sendable (MediaBlobReference) async throws -> Data

public enum MediaLoadingError: Error, LocalizedError, Sendable {
    case blobLoaderRequired
    case byteLengthMismatch
    case digestMismatch
    case invalidInlineData
    case unreadableImage

    public var errorDescription: String? {
        switch self {
        case .blobLoaderRequired:
            "Media blob requires the existing Host's authorized loader"
        case .byteLengthMismatch:
            "Media blob byte length did not match its reference"
        case .digestMismatch:
            "Media blob SHA-256 did not match its reference"
        case .invalidInlineData:
            "Inline Media did not contain a valid bounded image"
        case .unreadableImage:
            "Media image could not be decoded"
        }
    }
}

/// Native AppKit/SwiftUI interpretation of the static Media component.
///
/// Local paths are valid only in this trusted filesystem-sharing renderer.
/// Blob loading is asynchronous and delegated to the existing Host route.
@MainActor
public struct MediaView: View {
    public let snapshot: UISnapshot
    public let blobLoader: MediaBlobLoader?
    public let onAction: (UIAction) -> Void
    public let onError: (Error) -> Void

    public init(
        snapshot: UISnapshot,
        blobLoader: MediaBlobLoader? = nil,
        onAction: @escaping (UIAction) -> Void,
        onError: @escaping (Error) -> Void = { _ in }
    ) {
        self.snapshot = snapshot
        self.blobLoader = blobLoader
        self.onAction = onAction
        self.onError = onError
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .media(media):
            MediaContent(
                nodeID: snapshot.root.id,
                revision: snapshot.revision,
                media: media,
                blobLoader: blobLoader,
                onAction: onAction,
                onError: onError
            )
        case .canvasPage, .markdownEditor, .menu, .page, .surface, .textBox, .tree, .unsupported:
            EmptyView()
        }
    }
}

@MainActor
private struct MediaContent: View {
    let nodeID: String
    let revision: Int
    let media: MediaSpec
    let blobLoader: MediaBlobLoader?
    let onAction: (UIAction) -> Void
    let onError: (Error) -> Void

    @State private var image: NSImage?

    var body: some View {
        Group {
            if let action = media.activate {
                Button {
                    onAction(UIAction(
                        nodeID: nodeID,
                        action: action,
                        kind: .activate
                    ))
                } label: {
                    imageContent
                }
                .buttonStyle(.plain)
            } else {
                imageContent
            }
        }
        .frame(width: pointSize.width, height: pointSize.height)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(media.alt)
        .accessibilityHidden(media.alt.isEmpty)
        .task(id: MediaLoadIdentity(source: media.source, revision: revision)) {
            image = nil
            do {
                let loaded = try await loadImage()
                try Task.checkCancellation()
                image = loaded
            } catch is CancellationError {
                // A newer revision owns the image state now.
            } catch {
                onError(error)
            }
        }
    }

    @ViewBuilder
    private var imageContent: some View {
        if let image {
            switch media.fit {
            case .contain:
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
            case .cover:
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .clipped()
            case .fill:
                Image(nsImage: image)
                    .resizable()
            }
        } else {
            Color.clear
                .overlay {
                    if !media.alt.isEmpty {
                        Text(media.alt)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }
        }
    }

    private var pointSize: CGSize {
        let size = media.resolvedPointSize
        return CGSize(width: size.w, height: size.h)
    }

    private func loadImage() async throws -> NSImage {
        let data: Data
        switch media.source {
        case let .path(path):
            guard let image = NSImage(contentsOfFile: path) else {
                throw MediaLoadingError.unreadableImage
            }
            return image
        case let .inline(_, base64):
            guard base64.utf8.count <= 349_528,
                  let decoded = Data(base64Encoded: base64),
                  decoded.base64EncodedString() == base64,
                  (1...262_144).contains(decoded.count)
            else {
                throw MediaLoadingError.invalidInlineData
            }
            data = decoded
        case let .blob(reference):
            guard let blobLoader else {
                throw MediaLoadingError.blobLoaderRequired
            }
            data = try await blobLoader(reference)
            guard data.count == reference.byteLength else {
                throw MediaLoadingError.byteLengthMismatch
            }
            let digest = SHA256.hash(data: data)
                .map { String(format: "%02x", $0) }
                .joined()
            guard digest == reference.sha256 else {
                throw MediaLoadingError.digestMismatch
            }
        }
        guard let image = NSImage(data: data) else {
            throw MediaLoadingError.unreadableImage
        }
        return image
    }
}

private struct MediaLoadIdentity: Hashable {
    let source: MediaSource
    let revision: Int
}
