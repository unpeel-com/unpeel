import SwiftUI
import UnpeelShared
import PhotosUI
#if os(iOS)
import UIKit
#endif

extension RemoteBrowserArtifact: Identifiable {
    /// Stable within a session: the kind + filename is the on-disk address.
    public var id: String { "\(kind)/\(name)" }
}

struct RemoteGalleryAttachmentPayload: Equatable {
    let data: Data
    let contentType: String
}

/// Resumable Hosts accept verified JPEG/PNG uploads only. Gallery artifacts
/// can also be GIF/WebP, so capable Hosts get a still-image transcode while
/// legacy Hosts keep receiving the original bytes and MIME exactly as before.
enum RemoteGalleryAttachmentNormalizer {
    private static let resumableContentTypes = ["image/jpeg", "image/png"]
    private static let maximumEdge: CGFloat = 2048
    private static let maximumUploadBytes = 4 * 1024 * 1024

    static func normalize(
        data: Data,
        contentType: String,
        forResumableUpload resumable: Bool
    ) -> RemoteGalleryAttachmentPayload? {
        guard resumable, !resumableContentTypes.contains(contentType) else {
            return RemoteGalleryAttachmentPayload(data: data, contentType: contentType)
        }
        guard let image = UIImage(data: data) else { return nil }

        let largestEdge = max(image.size.width, image.size.height)
        let scale = largestEdge > maximumEdge ? maximumEdge / largestEdge : 1
        let targetSize = CGSize(
            width: max(1, (image.size.width * scale).rounded()),
            height: max(1, (image.size.height * scale).rounded())
        )
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = false
        let rendered = UIGraphicsImageRenderer(size: targetSize, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: targetSize))
        }

        // Prefer lossless PNG (including transparency), but fall back to JPEG
        // when it would exceed the resumable protocol's total-size ceiling.
        if let png = rendered.pngData(), png.count <= maximumUploadBytes {
            return RemoteGalleryAttachmentPayload(data: png, contentType: "image/png")
        }
        guard let jpeg = rendered.jpegData(compressionQuality: 0.88),
              jpeg.count <= maximumUploadBytes
        else { return nil }
        return RemoteGalleryAttachmentPayload(data: jpeg, contentType: "image/jpeg")
    }
}

/// The per-session browser-MCP gallery: every screenshot (and download) the
/// agent's browser captured for this session, fetched over the same
/// LAN/relay pipe as the terminal. A screenshot can be applied back to the
/// message (attached to the composer). This is a view of the agent's own
/// output, not a file browser.
struct BrowserGalleryPanel: View {
    let client: RemoteMacClient
    let sessionID: String
    let supportsResumableUpload: Bool
    /// Apply a screenshot to the active session's composer (bytes +
    /// content type). Attaching also closes the gallery.
    var onApply: (Data, String) -> Void = { _, _ in }

    @State private var artifacts: [RemoteBrowserArtifact] = []
    @State private var images: [String: UIImage] = [:]
    @State private var loadState: LoadState = .loading
    @State private var selected: RemoteBrowserArtifact?
    @State private var pickerItem: PhotosPickerItem?
    @State private var uploading = false
    @State private var uploadError: String?

    private enum LoadState: Equatable {
        case loading
        case loaded
        case failed(String)
    }

    private let columns = Array(repeating: GridItem(.flexible(), spacing: 8), count: 3)

    var body: some View {
        ZStack {
            Color(hex: 0x16171C).ignoresSafeArea()
            VStack(spacing: 0) {
                header
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .environment(\.colorScheme, .dark)
        .onChange(of: pickerItem) { item in
            guard let item else { return }
            Task { await uploadPicked(item) }
        }
        .task(id: sessionID) { await reload() }
        .sheet(item: $selected) { artifact in
            BrowserArtifactDetailView(
                client: client,
                sessionID: sessionID,
                artifact: artifact,
                preloaded: images[key(artifact)],
                onApply: { data, contentType in
                    guard let payload = attachmentPayload(data, contentType: contentType) else {
                        uploadError = "Couldn't prepare this image for upload."
                        return
                    }
                    selected = nil
                    onApply(payload.data, payload.contentType)
                }
            )
            .environment(\.colorScheme, .dark)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Session gallery")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(.white)
                Spacer()
                if uploading {
                    ProgressView().tint(.white.opacity(0.7)).controlSize(.small)
                }
            }
            Text("Screenshots the agent's browser and device capture, plus images you add or mark up.")
                .font(.system(size: 12.5))
                .foregroundStyle(.white.opacity(0.5))
                .fixedSize(horizontal: false, vertical: true)
            if let uploadError {
                Text(uploadError)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.red.opacity(0.9))
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 18)
        .padding(.bottom, 10)
    }

    @ViewBuilder
    private var content: some View {
        if loadState == .loading, artifacts.isEmpty {
            ProgressView()
                .tint(.white)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            grid
        }
    }

    private var grid: some View {
        ScrollView {
            LazyVGrid(columns: columns, spacing: 8) {
                addImageTile
                ForEach(artifacts, id: \.self) { artifact in
                    BrowserArtifactThumbnail(
                        artifact: artifact,
                        image: images[key(artifact)]
                    )
                    .onTapGesture { selected = artifact }
                    .task(id: key(artifact)) { await loadThumbnail(artifact) }
                    .contextMenu {
                        if isImage(artifact) {
                            Button {
                                Task { await applyArtifact(artifact) }
                            } label: {
                                Label("Add to message", systemImage: "paperclip")
                            }
                        }
                        Button { selected = artifact } label: {
                            Label("Open", systemImage: "arrow.up.left.and.arrow.down.right")
                        }
                        Divider()
                        Button(role: .destructive) {
                            Task { await deleteArtifact(artifact) }
                        } label: {
                            Label("Delete", systemImage: "trash")
                        }
                    }
                }
            }
            .padding(EdgeInsets(top: 14, leading: 12, bottom: 24, trailing: 12))
        }
        .refreshable { await reload() }
    }

    /// First tile: pick an image from the photo library and upload it into
    /// this session's gallery (and it becomes attachable like any other).
    private var addImageTile: some View {
        // Same square structure as BrowserArtifactThumbnail so it can't be a
        // different height and inflate its grid row.
        PhotosPicker(selection: $pickerItem, matching: .images) {
            VStack(spacing: 5) {
                Image(systemName: "plus")
                    .font(.system(size: 22, weight: .semibold))
                Text("Add image")
                    .font(.system(size: 11, weight: .medium))
            }
            .foregroundStyle(.white.opacity(0.7))
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .aspectRatio(1, contentMode: .fit)
            .background(Color.white.opacity(0.06))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(
                        .white.opacity(0.22),
                        style: StrokeStyle(lineWidth: 1, dash: [5, 4])
                    )
            )
        }
        .disabled(uploading)
    }

    /// Normalize any picked image (HEIC/PNG/…) and upload it into the session,
    /// then refresh so it appears in the grid. Surfaces failures instead of
    /// silently doing nothing.
    private func uploadPicked(_ item: PhotosPickerItem) async {
        defer { pickerItem = nil }
        uploading = true
        uploadError = nil
        defer { uploading = false }
        do {
            guard let data = try await item.loadTransferable(type: Data.self) else {
                uploadError = "Couldn't read the selected image."
                return
            }
            // Re-encode decodable images to JPEG; upload anything else as-is.
            let payload: Data
            let contentType: String
            if let uiImage = UIImage(data: data),
               let jpeg = uiImage.jpegData(compressionQuality: 0.9) {
                payload = jpeg
                contentType = "image/jpeg"
            } else {
                payload = data
                contentType = Array(data.prefix(4)) == [0x89, 0x50, 0x4E, 0x47]
                    ? "image/png" : "image/jpeg"
            }
            let path = try await client.uploadImage(
                sessionID: sessionID,
                data: payload,
                contentType: contentType,
                resumable: supportsResumableUpload
            )
            await reload()
            // Open the freshly-added image. Match it by its server-generated
            // filename and preload the bytes we already have so it shows
            // instantly instead of re-fetching.
            let name = (path as NSString).lastPathComponent
            if let added = artifacts.first(where: { $0.kind == "uploads" && $0.name == name }) {
                if let uiImage = UIImage(data: payload) {
                    images[key(added)] = uiImage
                }
                selected = added
            }
        } catch {
            uploadError = (error as? RemoteMacClientError)?.description
                ?? error.localizedDescription
        }
    }

    private func deleteArtifact(_ artifact: RemoteBrowserArtifact) async {
        do {
            try await client.deleteArtifact(
                sessionID: sessionID,
                kind: artifact.kind,
                name: artifact.name
            )
            images[key(artifact)] = nil
            await reload()
        } catch {
            uploadError = (error as? RemoteMacClientError)?.description
                ?? error.localizedDescription
        }
    }

    private func key(_ artifact: RemoteBrowserArtifact) -> String {
        "\(artifact.kind)/\(artifact.name)"
    }

    private func isImage(_ artifact: RemoteBrowserArtifact) -> Bool {
        ["png", "jpg", "jpeg", "gif", "webp"].contains(
            (artifact.name as NSString).pathExtension.lowercased()
        )
    }

    private func reload() async {
        loadState = .loading
        do {
            let list = try await client.browserArtifacts(sessionID: sessionID)
            artifacts = list.artifacts
            loadState = .loaded
        } catch {
            loadState = .failed((error as? RemoteMacClientError)?.description ?? error.localizedDescription)
        }
    }

    /// Fetch an artifact's bytes and apply them to the composer (closing the
    /// gallery). Used by the grid context menu's quick "Add to message".
    private func applyArtifact(_ artifact: RemoteBrowserArtifact) async {
        guard let (data, contentType) = try? await client.browserArtifactData(
            sessionID: sessionID,
            kind: artifact.kind,
            name: artifact.name
        ) else { return }
        guard let payload = attachmentPayload(data, contentType: contentType) else {
            uploadError = "Couldn't prepare this image for upload."
            return
        }
        onApply(payload.data, payload.contentType)
    }

    private func attachmentPayload(
        _ data: Data,
        contentType: String
    ) -> RemoteGalleryAttachmentPayload? {
        RemoteGalleryAttachmentNormalizer.normalize(
            data: data,
            contentType: contentType,
            forResumableUpload: supportsResumableUpload
        )
    }

    private func loadThumbnail(_ artifact: RemoteBrowserArtifact) async {
        let cacheKey = key(artifact)
        guard images[cacheKey] == nil, isImage(artifact) else { return }
        // Ask for a grid-sized JPEG, not the original — a full-page screenshot
        // is multi-megabyte and takes many relay round-trips; the thumbnail is
        // one. Tapping through still fetches the original bytes.
        guard let (data, _) = try? await client.browserArtifactData(
            sessionID: sessionID,
            kind: artifact.kind,
            name: artifact.name,
            maxDimension: 512
        ), let image = UIImage(data: data) else { return }
        images[cacheKey] = image
    }
}

private struct BrowserArtifactThumbnail: View {
    let artifact: RemoteBrowserArtifact
    let image: UIImage?

    var body: some View {
        // Explicit square: the GeometryReader is squared by `aspectRatio`, and
        // the image is framed to that exact size + clipped, so it always fills
        // the tile edge-to-edge and can never overflow or render small.
        GeometryReader { geo in
            content
                .frame(width: geo.size.width, height: geo.size.height)
                .clipped()
        }
        .aspectRatio(1, contentMode: .fit)
        .background(Color.white.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(.white.opacity(0.08), lineWidth: 1)
        )
    }

    @ViewBuilder
    private var content: some View {
        if let image {
            Image(uiImage: image)
                .resizable()
                .scaledToFill()
        } else if isImage {
            ProgressView()
                .tint(.white.opacity(0.6))
        } else {
            VStack(spacing: 4) {
                Image(systemName: "arrow.down.doc")
                    .font(.system(size: 20, weight: .light))
                Text((artifact.name as NSString).pathExtension.uppercased())
                    .font(.system(size: 10, weight: .semibold))
            }
            .foregroundStyle(.white.opacity(0.5))
        }
    }

    private var isImage: Bool {
        ["png", "jpg", "jpeg", "gif", "webp"].contains(
            (artifact.name as NSString).pathExtension.lowercased()
        )
    }
}

/// Full-size view of one artifact with a share/save affordance. Reuses the
/// thumbnail bytes when the grid already fetched them.
private struct BrowserArtifactDetailView: View {
    let client: RemoteMacClient
    let sessionID: String
    let artifact: RemoteBrowserArtifact
    let preloaded: UIImage?
    let onApply: (Data, String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var image: UIImage?
    @State private var rawData: Data?
    @State private var contentType = "application/octet-stream"
    @State private var shareURL: URL?
    @State private var failed = false
    @State private var cropping = false
    @StateObject private var arrowMarkup = ArrowMarkup()
    @State private var arrowMode = false

    private var isImage: Bool {
        ["png", "jpg", "jpeg", "gif", "webp"].contains(
            (artifact.name as NSString).pathExtension.lowercased()
        )
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            // Bars above and below the image, never over it: the zoomable
            // canvas gets the space between them, so controls can't cover
            // any part of the screenshot.
            VStack(spacing: 0) {
                HStack {
                    Button { dismiss() } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 40, height: 40)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                    Spacer()
                    if let shareURL {
                        ShareLink(item: shareURL) {
                            Image(systemName: "square.and.arrow.up")
                                .font(.system(size: 15, weight: .semibold))
                                .foregroundStyle(.white)
                                .frame(width: 40, height: 40)
                                .background(.ultraThinMaterial, in: Circle())
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 8)

                imageArea
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                if isImage, image != nil {
                    if arrowMode {
                        arrowControls
                    } else {
                        HStack(spacing: 10) {
                            imageToolButton("Crop", systemImage: "crop") { cropping = true }
                            imageToolButton("Arrows", systemImage: "arrow.up.right") {
                                arrowMode = true
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 10)
                        .padding(.bottom, 10)
                    }

                    // Always available — attach with whatever markup exists,
                    // including mid-way through drawing arrows.
                    addToMessageButton
                }
            }
        }
        .task { await load() }
        .onChange(of: arrowMarkup.arrows) { _ in updateShareURL() }
        .fullScreenCover(isPresented: $cropping) {
            if let image {
                // Bake any arrows into the image first so crop keeps them.
                ImageCropView(
                    image: arrowMarkup.hasArrows ? arrowMarkup.flatten(onto: image) : image,
                    onDone: { edited in
                        applyEdited(edited)
                        arrowMarkup.clear()
                        cropping = false
                    },
                    onCancel: { cropping = false }
                )
            }
        }
    }

    /// The image region between the bars. The zoomable canvas is clipped to
    /// this frame so a zoomed/panned image never slides under the controls.
    @ViewBuilder
    private var imageArea: some View {
        if let image {
            ZoomableAnnotatableImageView(image: image, markup: arrowMarkup, arrowMode: arrowMode)
                .clipped()
        } else if failed {
            Text("Couldn't load this file")
                .font(.system(size: 14))
                .foregroundStyle(.white.opacity(0.6))
        } else {
            ProgressView().tint(.white)
        }
    }

    /// Draw-arrows controls, shown in place of the tools row while arrow mode
    /// is active: color swatches, undo/clear, and Done to exit the mode.
    private var arrowControls: some View {
        VStack(spacing: 12) {
            Text("Drag on the image to draw · pinch to zoom")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.white.opacity(0.6))
            HStack(spacing: 14) {
                ForEach(ArrowMarkup.palette, id: \.self) { hex in
                    Button { arrowMarkup.colorHex = hex } label: {
                        Circle()
                            .fill(Color(hex: hex))
                            .frame(width: 28, height: 28)
                            .overlay(
                                Circle().strokeBorder(
                                    .white.opacity(arrowMarkup.colorHex == hex ? 0.95 : 0.25),
                                    lineWidth: arrowMarkup.colorHex == hex ? 3 : 1
                                )
                            )
                    }
                    .accessibilityLabel("Color")
                }
            }
            HStack(spacing: 10) {
                imageToolButton("Undo", systemImage: "arrow.uturn.backward") { arrowMarkup.undo() }
                imageToolButton("Clear", systemImage: "trash") { arrowMarkup.clear() }
                imageToolButton("Done", systemImage: "checkmark") { arrowMode = false }
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 10)
    }

    private var addToMessageButton: some View {
        Button {
            guard let image else { return }
            // Bake arrows if any; otherwise attach the original bytes.
            if arrowMarkup.hasArrows, let png = arrowMarkup.flatten(onto: image).pngData() {
                onApply(png, "image/png")
            } else if let rawData {
                onApply(rawData, contentType)
            } else if let png = image.pngData() {
                onApply(png, "image/png")
            }
        } label: {
            Label("Add to message", systemImage: "paperclip")
                .font(.system(size: 15, weight: .semibold))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 14)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
                .overlay(
                    RoundedRectangle(cornerRadius: 14)
                        .strokeBorder(.white.opacity(0.14), lineWidth: 1)
                )
                .foregroundStyle(.white)
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 20)
    }

    /// Keep the Share item in sync with the current markup (flattened when
    /// there are arrows, the original bytes otherwise).
    private func updateShareURL() {
        guard let image else { return }
        if arrowMarkup.hasArrows, let png = arrowMarkup.flatten(onto: image).pngData() {
            let name = (artifact.name as NSString).deletingPathExtension + "-annotated.png"
            shareURL = try? Self.writeTemp(png, name: name)
        } else if let rawData {
            shareURL = try? Self.writeTemp(rawData, name: artifact.name)
        }
    }

    private func imageToolButton(
        _ title: String,
        systemImage: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(.system(size: 14, weight: .semibold))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 11)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .strokeBorder(.white.opacity(0.12), lineWidth: 1)
                )
                .foregroundStyle(.white)
        }
    }

    /// Replace the working image with an annotated version. The edit becomes a
    /// PNG (lossless, preserves the strokes) and drives both "Add to message"
    /// and Share from here on.
    private func applyEdited(_ edited: UIImage) {
        image = edited
        if let png = edited.pngData() {
            rawData = png
            contentType = "image/png"
            let editedName = (artifact.name as NSString).deletingPathExtension + "-annotated.png"
            shareURL = try? Self.writeTemp(png, name: editedName)
        }
    }

    private func load() async {
        // Fetch the real bytes even when a thumbnail is preloaded, so
        // "Add to message" attaches the original screenshot (right format,
        // full resolution) rather than a re-encode.
        if let preloaded { image = preloaded }
        do {
            let (data, type) = try await client.browserArtifactData(
                sessionID: sessionID,
                kind: artifact.kind,
                name: artifact.name
            )
            rawData = data
            contentType = type
            if let decoded = UIImage(data: data) { image = decoded }
            shareURL = try? Self.writeTemp(data, name: artifact.name)
            if image == nil { failed = true }
        } catch {
            if image == nil { failed = true }
        }
    }

    /// Stage the bytes in a temp file so ShareLink can hand a real, named
    /// file to Photos / Files / other apps.
    private static func writeTemp(_ data: Data?, name: String) throws -> URL? {
        guard let data else { return nil }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        try data.write(to: url, options: .atomic)
        return url
    }
}

/// Arrow markup drawn directly on the image canvas. Arrows are stored in
/// NORMALIZED image coordinates (0…1) so they're resolution-independent — the
/// on-screen canvas maps them to its bounds, and `flatten` maps them to the
/// image's native pixels. Shared between the SwiftUI detail view (tools/undo)
/// and the UIKit canvas (drawing/rendering).
final class ArrowMarkup: ObservableObject {
    struct Arrow: Identifiable, Equatable {
        let id = UUID()
        var start: CGPoint
        var end: CGPoint
        var colorHex: UInt32
    }

    @Published var arrows: [Arrow] = []
    @Published var colorHex: UInt32 = 0xEF4444  // red

    var hasArrows: Bool { !arrows.isEmpty }
    func undo() { if !arrows.isEmpty { arrows.removeLast() } }
    func clear() { arrows.removeAll() }

    static let palette: [UInt32] = [0xEF4444, 0xEAB308, 0x22C55E, 0x3B82F6, 0xFFFFFF]

    static func lineWidth(for width: CGFloat) -> CGFloat { max(2.5, width * 0.009) }
    static func headLength(for width: CGFloat) -> CGFloat { max(12, width * 0.038) }
    static func uiColor(_ hex: UInt32) -> UIColor {
        UIColor(
            red: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1
        )
    }

    /// Composite the arrows onto `image` at its native resolution.
    func flatten(onto image: UIImage) -> UIImage {
        guard !arrows.isEmpty else { return image }
        let size = image.size
        let format = UIGraphicsImageRendererFormat()
        format.scale = image.scale
        format.opaque = false
        return UIGraphicsImageRenderer(size: size, format: format).image { context in
            image.draw(in: CGRect(origin: .zero, size: size))
            let cg = context.cgContext
            cg.setLineCap(.round)
            cg.setLineJoin(.round)
            cg.setLineWidth(Self.lineWidth(for: size.width))
            let head = Self.headLength(for: size.width)
            for arrow in arrows {
                let start = CGPoint(x: arrow.start.x * size.width, y: arrow.start.y * size.height)
                let end = CGPoint(x: arrow.end.x * size.width, y: arrow.end.y * size.height)
                cg.setStrokeColor(Self.uiColor(arrow.colorHex).cgColor)
                cg.addPath(ArrowGeometry.path(from: start, to: end, headLength: head).cgPath)
                cg.strokePath()
            }
        }
    }
}

/// Transparent overlay that strokes arrows from normalized coords into its
/// bounds. Sits inside the zoomable content view, so arrows scale/pan with the
/// image for free.
private final class ArrowCanvasView: UIView {
    var arrows: [ArrowMarkup.Arrow] = [] { didSet { setNeedsDisplay() } }
    var liveArrow: ArrowMarkup.Arrow? { didSet { setNeedsDisplay() } }

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .clear
        isOpaque = false
        isUserInteractionEnabled = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) unavailable") }

    override func draw(_ rect: CGRect) {
        guard let cg = UIGraphicsGetCurrentContext(), bounds.width > 0 else { return }
        cg.setLineCap(.round)
        cg.setLineJoin(.round)
        cg.setLineWidth(ArrowMarkup.lineWidth(for: bounds.width))
        let head = ArrowMarkup.headLength(for: bounds.width)
        func stroke(_ arrow: ArrowMarkup.Arrow) {
            let start = CGPoint(x: arrow.start.x * bounds.width, y: arrow.start.y * bounds.height)
            let end = CGPoint(x: arrow.end.x * bounds.width, y: arrow.end.y * bounds.height)
            cg.setStrokeColor(ArrowMarkup.uiColor(arrow.colorHex).cgColor)
            cg.addPath(ArrowGeometry.path(from: start, to: end, headLength: head).cgPath)
            cg.strokePath()
        }
        for arrow in arrows { stroke(arrow) }
        if let liveArrow { stroke(liveArrow) }
    }
}

/// Pinch-to-zoom + pan for the full-size artifact image, WITH inline arrow
/// markup. Backed by a `UIScrollView`; the zooming content is a container
/// holding the image and an arrow overlay, so arrows stay pinned to the image
/// through zoom/pan. In arrow mode a single-finger drag draws an arrow while
/// two fingers still pan and pinch still zooms.
private struct ZoomableAnnotatableImageView: UIViewRepresentable {
    let image: UIImage
    @ObservedObject var markup: ArrowMarkup
    var arrowMode: Bool

    func makeUIView(context: Context) -> AnnotatableScrollView {
        let scrollView = AnnotatableScrollView()
        scrollView.configure(coordinator: context.coordinator)
        scrollView.imageView.image = image
        context.coordinator.scrollView = scrollView
        return scrollView
    }

    func updateUIView(_ scrollView: AnnotatableScrollView, context: Context) {
        if scrollView.imageView.image !== image {
            scrollView.imageView.image = image
            // The new image (e.g. a crop) can have a different aspect ratio —
            // re-fit the content to it, or `scaleToFill` stretches it into the
            // old fitted rect.
            scrollView.refit()
        }
        context.coordinator.markup = markup
        scrollView.canvasView.arrows = markup.arrows
        scrollView.setArrowMode(arrowMode)
    }

    func makeCoordinator() -> Coordinator { Coordinator(markup: markup) }

    final class Coordinator: NSObject, UIScrollViewDelegate {
        weak var scrollView: AnnotatableScrollView?
        var markup: ArrowMarkup
        private var dragStart: CGPoint?

        init(markup: ArrowMarkup) { self.markup = markup }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? {
            (scrollView as? AnnotatableScrollView)?.contentView
        }

        func scrollViewDidZoom(_ scrollView: UIScrollView) {
            (scrollView as? AnnotatableScrollView)?.centerContent()
        }

        private func normalized(_ point: CGPoint, in view: UIView) -> CGPoint {
            CGPoint(
                x: min(max(point.x / view.bounds.width, 0), 1),
                y: min(max(point.y / view.bounds.height, 0), 1)
            )
        }

        @objc func handleDraw(_ gesture: UIPanGestureRecognizer) {
            guard let canvas = scrollView?.canvasView, canvas.bounds.width > 0 else { return }
            let point = normalized(gesture.location(in: canvas), in: canvas)
            let hex = markup.colorHex
            switch gesture.state {
            case .began:
                dragStart = point
                canvas.liveArrow = .init(start: point, end: point, colorHex: hex)
            case .changed:
                if let start = dragStart {
                    canvas.liveArrow = .init(start: start, end: point, colorHex: hex)
                }
            case .ended:
                if let start = dragStart {
                    let dx = (point.x - start.x) * canvas.bounds.width
                    let dy = (point.y - start.y) * canvas.bounds.height
                    if hypot(dx, dy) > 8 {
                        let arrow = ArrowMarkup.Arrow(start: start, end: point, colorHex: hex)
                        markup.arrows.append(arrow)
                        canvas.arrows = markup.arrows
                    }
                }
                dragStart = nil
                canvas.liveArrow = nil
            default:
                dragStart = nil
                canvas.liveArrow = nil
            }
        }

        @objc func handleDoubleTap(_ gesture: UITapGestureRecognizer) {
            guard let scrollView, let content = scrollView.viewForZooming else { return }
            if scrollView.zoomScale > scrollView.minimumZoomScale {
                scrollView.setZoomScale(scrollView.minimumZoomScale, animated: true)
            } else {
                let point = gesture.location(in: content)
                let scale = min(scrollView.maximumZoomScale, 3)
                let width = scrollView.bounds.width / scale
                let height = scrollView.bounds.height / scale
                scrollView.zoom(
                    to: CGRect(x: point.x - width / 2, y: point.y - height / 2, width: width, height: height),
                    animated: true
                )
            }
        }
    }
}

/// UIScrollView whose zooming content is `contentView` (image + arrow overlay),
/// fitted to bounds in `layoutSubviews` so the first present snaps straight to
/// the fitted frame with no "scale into place" animation.
private final class AnnotatableScrollView: UIScrollView, UIGestureRecognizerDelegate {
    let contentView = UIView()
    let imageView = UIImageView()
    let canvasView = ArrowCanvasView()
    private let drawGesture = UIPanGestureRecognizer()
    private var lastFittedBounds: CGRect = .zero

    var viewForZooming: UIView? { contentView }

    func configure(coordinator: ZoomableAnnotatableImageView.Coordinator) {
        delegate = coordinator
        minimumZoomScale = 1
        maximumZoomScale = 5
        bouncesZoom = true
        showsVerticalScrollIndicator = false
        showsHorizontalScrollIndicator = false
        backgroundColor = .clear
        contentInsetAdjustmentBehavior = .never

        imageView.contentMode = .scaleToFill
        contentView.addSubview(imageView)
        contentView.addSubview(canvasView)
        addSubview(contentView)

        drawGesture.addTarget(coordinator, action: #selector(ZoomableAnnotatableImageView.Coordinator.handleDraw(_:)))
        drawGesture.maximumNumberOfTouches = 1
        drawGesture.isEnabled = false
        addGestureRecognizer(drawGesture)

        let doubleTap = UITapGestureRecognizer(
            target: coordinator,
            action: #selector(ZoomableAnnotatableImageView.Coordinator.handleDoubleTap(_:))
        )
        doubleTap.numberOfTapsRequired = 2
        addGestureRecognizer(doubleTap)
    }

    /// Arrow mode: single-finger drag draws (drawGesture on), while two fingers
    /// still pan (raise the scroll pan's min touches) and pinch still zooms.
    func setArrowMode(_ on: Bool) {
        drawGesture.isEnabled = on
        panGestureRecognizer.minimumNumberOfTouches = on ? 2 : 1
    }

    /// Re-fit to the current image (call after swapping in an image with a
    /// different aspect ratio, e.g. a crop result).
    func refit() {
        lastFittedBounds = .zero
        setZoomScale(1, animated: false)
        setNeedsLayout()
        layoutIfNeeded()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) unavailable") }

    override init(frame: CGRect) { super.init(frame: frame) }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard let image = imageView.image, bounds.width > 0, bounds.height > 0 else { return }
        if bounds != lastFittedBounds, zoomScale == minimumZoomScale {
            lastFittedBounds = bounds
            let fitted = ImageAnnotationView.fittedSize(image.size, in: bounds.size)
            UIView.performWithoutAnimation {
                contentView.frame = CGRect(origin: .zero, size: fitted)
                imageView.frame = contentView.bounds
                canvasView.frame = contentView.bounds
                contentSize = fitted
                centerContent()
            }
        } else {
            centerContent()
        }
    }

    /// Center the (possibly-zoomed) content via symmetric inset.
    func centerContent() {
        let insetX = max(0, (bounds.width - contentSize.width) / 2)
        let insetY = max(0, (bounds.height - contentSize.height) / 2)
        contentInset = UIEdgeInsets(top: insetY, left: insetX, bottom: insetY, right: insetX)
    }
}
