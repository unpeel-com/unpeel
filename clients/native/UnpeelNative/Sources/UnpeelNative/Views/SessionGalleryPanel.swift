//
//  SessionGalleryPanel.swift
//  UnpeelNative
//
//  Desktop session gallery: every image the session's agent captured
//  (browser/computer screenshots, downloads) plus images added from the
//  phone, read straight from the session's on-disk artifacts
//  (SessionArtifactStore) — the desktop twin of the iOS
//  BrowserGalleryPanel. Opened from the photo button at the trailing edge
//  of the terminal title bar (TerminalArea). This is a view of the agent's
//  own output, not a file browser.
//

import AppKit
import ImageIO
import SwiftUI

/// Title-bar chip that opens the gallery popover, split like
/// WorkspaceOpenMenu (styled the same glass chip so the two sit together
/// naturally): the photo side opens the gallery, the chevron side drops a
/// screenshot menu (capture area / window / full screen into the session's
/// uploads).
struct SessionGalleryButton: View {
    let sessionID: String
    let cache: SurfaceCache
    /// Ghost presentation for the pane's top-right corner group: bare
    /// glyphs, muted → foreground on hover, no chip material or fill at
    /// rest, matching the pane control glyphs. Behavior is identical.
    var ghost = false

    @State private var showingGallery = false
    @State private var hovering = false
    @State private var chevronHovering = false
    @State private var pulsing = false
    @State private var screenshotMenu = SessionScreenshotMenuController()
    @State private var revealOnOpen: URL?

    var body: some View {
        HStack(spacing: 0) {
            Button {
                pulsing = false
                revealOnOpen = nil
                showingGallery.toggle()
            } label: {
                Image(systemName: "photo.on.rectangle")
                    .font(.system(size: ghost ? 10.5 : 11.5, weight: ghost ? .semibold : .medium))
                    .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: ghost ? 24 : 32, height: ghost ? 22 : 26)
                    .background(hovering && !ghost ? Theme.hoverRow.opacity(0.45) : .clear)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .help("Session gallery")
            .accessibilityLabel("Session gallery")
            .popover(isPresented: $showingGallery, arrowEdge: .bottom) {
                SessionGalleryPanel(
                    sessionID: sessionID,
                    cache: cache,
                    revealOnOpen: revealOnOpen,
                    onClose: { showingGallery = false }
                )
                // The popover reuses its content view's state storage across
                // presentations, which silently discards the panel's
                // seeded-at-init selection. A new identity per screenshot
                // guarantees the fresh shot opens on its detail view.
                .id(revealOnOpen)
            }

            if !ghost {
                Rectangle()
                    .fill(Theme.foreground.opacity(0.10))
                    .frame(width: 1, height: 14)
                    .allowsHitTesting(false)
            }

            Button {
                screenshotMenu.onCapture = { mode in startCapture(mode) }
                screenshotMenu.present()
            } label: {
                Image(systemName: "chevron.down")
                    .font(.system(size: ghost ? 8 : 9, weight: .semibold))
                    .foregroundStyle(chevronHovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: ghost ? 16 : 25, height: ghost ? 22 : 26)
                    .background(chevronHovering && !ghost ? Theme.hoverRow.opacity(0.45) : .clear)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(SessionScreenshotMenuAnchor(controller: screenshotMenu))
            .onHover { chevronHovering = $0 }
            .help("Take screenshot")
            .accessibilityLabel("Take screenshot")
        }
        .frame(height: ghost ? 22 : 26)
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .modifier(GalleryChipBackground(enabled: !ghost))
        .scaleEffect(pulsing ? 1.08 : 1)
        .animation(.spring(response: 0.3, dampingFraction: 0.5), value: pulsing)
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .animation(.easeInOut(duration: 0.12), value: chevronHovering)
        .task(id: sessionID) { await watchForNewCaptures() }
        // Session ▸ Take Screenshot… (⌘⇧S): only the displayed session's
        // chip is mounted, so this targets the right session for free.
        .onReceive(
            NotificationCenter.default.publisher(for: .unpeelTakeSessionScreenshot)
        ) { _ in
            startCapture(.area)
        }
    }

    /// Take a screenshot and open the gallery on the fresh shot — its
    /// detail view carries Add to prompt / markup. Shared by the chevron
    /// dropdown and the ⌘⇧S menu item.
    private func startCapture(_ mode: SessionScreenshotCapture.Mode) {
        SessionScreenshotCapture.capture(mode, sessionID: sessionID) { url in
            guard let url else { return }
            revealOnOpen = url
            showingGallery = true
        }
    }

    /// The same pulse the iOS gallery button does (`watchForNewScreenshots`
    /// in RemoteGhosttyTerminalView — keep the two in step): poll the
    /// session's captures, pulse when one newer than the last-seen appears.
    /// The first sample only establishes the floor, so pre-existing
    /// screenshots never pulse; while the gallery is open the new tile is
    /// already visible, so arrivals are absorbed silently.
    @MainActor
    private func watchForNewCaptures() async {
        let sessionID = sessionID
        var baseline: Int64 = -1
        while !Task.isCancelled {
            let newest = await Task.detached(priority: .utility) {
                SessionArtifactStore.latestCaptureUnixMs(sessionID)
            }.value
            if baseline < 0 || showingGallery {
                baseline = max(baseline, newest)
            } else if newest > baseline {
                baseline = newest
                pulsing = true
                try? await Task.sleep(nanoseconds: 2_200_000_000)
                pulsing = false
            }
            try? await Task.sleep(nanoseconds: 2_000_000_000)
        }
    }
}

/// The same flat material treatment WorkspaceOpenMenu uses for its chip
/// (no Liquid Glass — its drop shadow clutters the titlebar strip).
private struct GalleryChipBackground: ViewModifier {
    var enabled = true

    @ViewBuilder
    func body(content: Content) -> some View {
        if enabled {
            content
                .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(Theme.foreground.opacity(0.08))
                )
        } else {
            content
        }
    }
}

struct SessionGalleryPanel: View {
    let sessionID: String
    let cache: SurfaceCache
    /// When set, the panel opens straight on this artifact's detail view
    /// (used right after a title-bar screenshot lands in uploads).
    var revealOnOpen: URL?
    var onClose: () -> Void = {}

    /// Resolve `revealOnOpen` synchronously so a fresh presentation renders
    /// the image detail as its very first frame — never the grid.
    init(
        sessionID: String,
        cache: SurfaceCache,
        revealOnOpen: URL? = nil,
        onClose: @escaping () -> Void = {}
    ) {
        self.sessionID = sessionID
        self.cache = cache
        self.revealOnOpen = revealOnOpen
        self.onClose = onClose
        if let revealOnOpen {
            let target = revealOnOpen.resolvingSymlinksInPath().path
            let list = SessionArtifactStore.list(sessionID)
            let hit = list.first(where: { $0.url.resolvingSymlinksInPath().path == target })
            if let hit {
                _artifacts = State(initialValue: list)
                _selected = State(initialValue: hit)
            }
        }
    }

    @State private var artifacts: [SessionArtifact] = []
    @State private var thumbnails: [String: NSImage] = [:]
    @State private var selected: SessionArtifact?
    @State private var detailImage: NSImage?

    // Markup state (arrows + crop, mirroring the iOS gallery detail view).
    // `workingCG` is the full-resolution bitmap all edits apply to; a crop
    // replaces it (baking any arrows first, like iOS), so `croppedSinceLoad`
    // plus pending arrows together mean "export an annotated copy".
    @State private var workingCG: CGImage?
    @State private var arrows: [GalleryArrow] = []
    @State private var liveArrow: GalleryArrow?
    @State private var arrowMode = false
    @State private var arrowColorHex: UInt32 = 0xEF4444
    @State private var cropMode = false
    @State private var cropSelection: CGRect?
    @State private var croppedSinceLoad = false

    private let columns = Array(repeating: GridItem(.flexible(), spacing: 8), count: 3)
    /// Live refresh while the popover is open — the agent may still be
    /// capturing. A metadata-only scan of a handful of dirs, so it's cheap.
    private let refresh = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        Group {
            if let selected {
                detail(selected)
            } else {
                gallery
            }
        }
        .frame(width: 460, height: 500)
        .task(id: sessionID) {
            reload()
            applyRevealOnOpen()
        }
        // .task does not re-run when a popover re-presents a kept-alive
        // content view; onAppear does fire per presentation.
        .onAppear {
            reload()
            applyRevealOnOpen()
        }
        .onChange(of: revealOnOpen) { _ in
            reload()
            applyRevealOnOpen()
        }
        .onReceive(refresh) { _ in reload() }
    }

    // MARK: - Grid

    private var gallery: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Session gallery")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.foreground)
                Spacer()
                if !artifacts.isEmpty {
                    Text("\(artifacts.count)")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Theme.mutedForeground)
                }
            }
            .padding(.horizontal, 14)
            .padding(.top, 12)
            .padding(.bottom, 8)

            if artifacts.isEmpty {
                emptyState
            } else {
                ScrollView {
                    LazyVGrid(columns: columns, spacing: 8) {
                        ForEach(artifacts) { artifact in
                            GalleryTile(artifact: artifact, image: thumbnails[artifact.id])
                                .onTapGesture { open(artifact) }
                                .task(id: artifact.id) { await loadThumbnail(artifact) }
                                .contextMenu { artifactMenu(artifact) }
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 12)
                }
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "photo.on.rectangle")
                .font(.system(size: 28, weight: .light))
                .foregroundStyle(Theme.mutedForeground)
            Text("No images yet")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
            Text("Screenshots the agent's browser and computer tools capture — and images added from your phone — show up here.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.foreground.opacity(0.4))
                .multilineTextAlignment(.center)
                .padding(.horizontal, 40)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Detail

    private func detail(_ artifact: SessionArtifact) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Button {
                    closeDetail()
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.foreground)
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back to gallery")
                Text(artifact.name)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.foreground)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 8)
                Text(artifact.modifiedAt.formatted(date: .abbreviated, time: .shortened))
                    .font(.system(size: 10.5))
                    .foregroundStyle(Theme.mutedForeground)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)

            ZStack {
                if let detailImage, let workingCG {
                    GalleryAnnotatableImage(
                        image: detailImage,
                        pixelSize: CGSize(width: workingCG.width, height: workingCG.height),
                        arrows: arrows,
                        liveArrow: liveArrow,
                        cropSelection: cropMode ? cropSelection : nil,
                        interactive: arrowMode || cropMode,
                        onDrag: handleMarkupDrag
                    )
                } else if artifact.isImage {
                    ProgressView()
                        .controlSize(.small)
                } else {
                    VStack(spacing: 6) {
                        Image(systemName: "arrow.down.doc")
                            .font(.system(size: 26, weight: .light))
                        Text((artifact.name as NSString).pathExtension.uppercased())
                            .font(.system(size: 11, weight: .semibold))
                    }
                    .foregroundStyle(Theme.mutedForeground)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(.horizontal, 12)
            .task(id: artifact.id) { await loadDetail(artifact) }

            if workingCG != nil {
                markupControls
            }

            HStack(spacing: 8) {
                if canAddToPrompt {
                    EditorButton(title: "Add to prompt", variant: .primary, size: .small) {
                        addToPrompt(artifact)
                    }
                }
                GalleryActionButton(title: "Reveal in Finder", systemImage: "magnifyingglass") {
                    NSWorkspace.shared.activateFileViewerSelecting([artifact.url])
                }
                Spacer(minLength: 0)
                GalleryActionButton(title: "Delete", systemImage: "trash", destructive: true) {
                    delete(artifact)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
        }
    }

    // MARK: - Markup (arrows + crop)

    /// The tools strip between image and footer: mode entry when idle,
    /// palette + undo/clear/done in arrow mode, apply/cancel in crop mode.
    @ViewBuilder
    private var markupControls: some View {
        Group {
            if arrowMode {
                HStack(spacing: 8) {
                    ForEach(GalleryMarkup.palette, id: \.self) { hex in
                        Button {
                            arrowColorHex = hex
                        } label: {
                            Circle()
                                .fill(GalleryMarkup.color(hex))
                                .frame(width: 16, height: 16)
                                .overlay(
                                    Circle().strokeBorder(
                                        Theme.foreground.opacity(arrowColorHex == hex ? 0.95 : 0.25),
                                        lineWidth: arrowColorHex == hex ? 2 : 1
                                    )
                                )
                                .contentShape(Circle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Arrow color")
                    }
                    Text("Drag on the image to draw")
                        .font(.system(size: 10.5))
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    GalleryActionButton(title: "Undo", systemImage: "arrow.uturn.backward") {
                        if !arrows.isEmpty { arrows.removeLast() }
                    }
                    GalleryActionButton(title: "Done", systemImage: "checkmark") {
                        arrowMode = false
                    }
                }
            } else if cropMode {
                HStack(spacing: 8) {
                    Text("Drag on the image to select the crop")
                        .font(.system(size: 10.5))
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    GalleryActionButton(title: "Apply", systemImage: "checkmark") {
                        applyCrop()
                    }
                    GalleryActionButton(title: "Cancel", systemImage: "xmark") {
                        cropMode = false
                        cropSelection = nil
                    }
                }
            } else {
                HStack(spacing: 8) {
                    GalleryActionButton(title: "Arrows", systemImage: "arrow.up.right") {
                        arrowMode = true
                    }
                    GalleryActionButton(title: "Crop", systemImage: "crop") {
                        cropMode = true
                        cropSelection = nil
                    }
                    if !arrows.isEmpty {
                        GalleryActionButton(title: "Clear markup", systemImage: "trash") {
                            arrows.removeAll()
                        }
                    }
                    Spacer(minLength: 0)
                    if hasEdits {
                        Text("Add to prompt attaches the annotated copy")
                            .font(.system(size: 10.5))
                            .foregroundStyle(Theme.mutedForeground)
                            .lineLimit(1)
                    }
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 8)
    }

    private var hasEdits: Bool {
        croppedSinceLoad || !arrows.isEmpty
    }

    /// One drag stream, interpreted per mode: crop drags rubber-band the
    /// selection; arrow drags preview live and commit on release (same 8pt
    /// minimum as iOS, measured in fitted points).
    private func handleMarkupDrag(
        start: CGPoint,
        current: CGPoint,
        fitted: CGSize,
        ended: Bool
    ) {
        if cropMode {
            cropSelection = CGRect(
                x: min(start.x, current.x),
                y: min(start.y, current.y),
                width: abs(current.x - start.x),
                height: abs(current.y - start.y)
            )
        } else if arrowMode {
            if ended {
                liveArrow = nil
                let dx = (current.x - start.x) * fitted.width
                let dy = (current.y - start.y) * fitted.height
                if hypot(dx, dy) > 8 {
                    arrows.append(
                        GalleryArrow(start: start, end: current, colorHex: arrowColorHex)
                    )
                }
            } else {
                liveArrow = GalleryArrow(start: start, end: current, colorHex: arrowColorHex)
            }
        }
    }

    /// Commit the crop selection: bake any arrows first (so they survive the
    /// crop, like iOS), then swap the working bitmap for the cropped one.
    private func applyCrop() {
        defer {
            cropMode = false
            cropSelection = nil
        }
        guard let cg = workingCG,
              let rect = cropSelection,
              rect.width > 0.01, rect.height > 0.01
        else { return }
        let baked = GalleryMarkup.flatten(cg, arrows: arrows)
        guard let cropped = GalleryMarkup.crop(baked, normalizedRect: rect) else { return }
        workingCG = cropped
        detailImage = NSImage(cgImage: cropped, size: .zero)
        arrows = []
        croppedSinceLoad = true
    }

    private func closeDetail() {
        selected = nil
        resetMarkup()
    }

    /// Jump straight to the detail view of the artifact `revealOnOpen`
    /// points at (a screenshot that just landed). No-op when it isn't in
    /// the loaded list (yet) or is already showing.
    private func applyRevealOnOpen() {
        guard let revealOnOpen else { return }
        let target = revealOnOpen.resolvingSymlinksInPath().path
        guard selected?.url.resolvingSymlinksInPath().path != target else { return }
        guard let artifact = artifacts.first(where: {
            $0.url.resolvingSymlinksInPath().path == target
        }) else { return }
        open(artifact)
    }

    private func resetMarkup() {
        detailImage = nil
        workingCG = nil
        arrows = []
        liveArrow = nil
        arrowMode = false
        cropMode = false
        cropSelection = nil
        croppedSinceLoad = false
    }

    /// Flatten the current edits at full resolution and save them as a new
    /// `uploads/` artifact (the same kind phone-annotated images land in), so
    /// the annotated copy shows up in the gallery and the original stays
    /// untouched. Returns nil when there's nothing to export or saving fails.
    private func exportEditedCopy(of artifact: SessionArtifact) -> URL? {
        guard hasEdits, let cg = workingCG else { return nil }
        let final = GalleryMarkup.flatten(cg, arrows: arrows)
        guard let data = GalleryMarkup.pngData(final),
              let dir = SessionArtifactStore.kindDir(sessionID, kind: "uploads")
        else { return nil }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let base = (artifact.name as NSString).deletingPathExtension
        let stamp = UInt64(Date().timeIntervalSince1970 * 1000)
        let url = dir.appendingPathComponent("\(base)-annotated-\(stamp).png")
        do {
            try data.write(to: url, options: .atomic)
        } catch {
            return nil
        }
        return url
    }

    @ViewBuilder
    private func artifactMenu(_ artifact: SessionArtifact) -> some View {
        if canAddToPrompt {
            Button {
                addToPrompt(artifact)
            } label: {
                Label("Add to prompt", systemImage: "paperclip")
            }
        }
        Button {
            open(artifact)
        } label: {
            Label("Open", systemImage: "arrow.up.left.and.arrow.down.right")
        }
        Button {
            NSWorkspace.shared.activateFileViewerSelecting([artifact.url])
        } label: {
            Label("Reveal in Finder", systemImage: "magnifyingglass")
        }
        Divider()
        Button(role: .destructive) {
            delete(artifact)
        } label: {
            Label("Delete", systemImage: "trash")
        }
    }

    // MARK: - Actions

    /// The displayed session's terminal pane exists whenever the session is
    /// live and mounted; a dead/archived session has nowhere to type.
    private var canAddToPrompt: Bool {
        cache.existingPane(for: sessionID) != nil
    }

    /// Type the artifact's path into the session's prompt, exactly like
    /// dropping the file on the terminal, and close the gallery. When the
    /// image carries markup (arrows/crop), an annotated copy is exported
    /// first and its path is attached instead of the original.
    private func addToPrompt(_ artifact: SessionArtifact) {
        guard let pane = cache.existingPane(for: sessionID) else { return }
        let url = exportEditedCopy(of: artifact) ?? artifact.url
        pane.insertAttachablePath(url.path)
        onClose()
    }

    /// Images enlarge in place; anything else (downloads: PDFs, files)
    /// opens in its default app.
    private func open(_ artifact: SessionArtifact) {
        if artifact.isImage {
            selected = artifact
        } else {
            NSWorkspace.shared.open(artifact.url)
        }
    }

    private func delete(_ artifact: SessionArtifact) {
        try? SessionArtifactStore.delete(sessionID, kind: artifact.kind, name: artifact.name)
        thumbnails[artifact.id] = nil
        if selected?.id == artifact.id {
            closeDetail()
        }
        reload()
    }

    // MARK: - Loading

    private func reload() {
        let list = SessionArtifactStore.list(sessionID)
        guard list != artifacts else { return }
        artifacts = list
        let ids = Set(list.map(\.id))
        thumbnails = thumbnails.filter { ids.contains($0.key) }
        if let selected, !ids.contains(selected.id) {
            closeDetail()
        }
    }

    private func loadThumbnail(_ artifact: SessionArtifact) async {
        guard artifact.isImage, thumbnails[artifact.id] == nil else { return }
        let url = artifact.url
        let cgImage = await Task.detached(priority: .utility) {
            SessionArtifactImageLoader.decode(url: url, maxDim: 480)
        }.value
        guard let cgImage, artifacts.contains(where: { $0.id == artifact.id }) else { return }
        thumbnails[artifact.id] = NSImage(cgImage: cgImage, size: .zero)
    }

    /// Loads the working bitmap for the detail view. Near-native resolution
    /// (capped for memory sanity) because it's also the base every markup
    /// export flattens onto — a grid-thumbnail decode here would make
    /// annotated exports a downgrade.
    private func loadDetail(_ artifact: SessionArtifact) async {
        resetMarkup()
        guard artifact.isImage else { return }
        let url = artifact.url
        let cgImage = await Task.detached(priority: .userInitiated) {
            SessionArtifactImageLoader.decode(url: url, maxDim: 4096)
        }.value
        guard let cgImage, selected?.id == artifact.id else { return }
        workingCG = cgImage
        detailImage = NSImage(cgImage: cgImage, size: .zero)
    }
}

/// Downsampled image decode for gallery tiles and the detail view, safe to
/// call off the main actor.
enum SessionArtifactImageLoader {
    nonisolated static func decode(url: URL, maxDim: Int) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(
            url as CFURL,
            [kCGImageSourceShouldCache: false] as CFDictionary
        ) else { return nil }
        let options = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxDim,
        ] as CFDictionary
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options)
    }
}

/// Square grid tile: image fills edge-to-edge; non-image artifacts show a
/// download-doc glyph with the extension (same anatomy as the iOS tiles).
private struct GalleryTile: View {
    let artifact: SessionArtifact
    let image: NSImage?

    var body: some View {
        ZStack {
            if let image {
                GeometryReader { geo in
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFill()
                        .frame(width: geo.size.width, height: geo.size.height)
                        .clipped()
                }
            } else if artifact.isImage {
                ProgressView()
                    .controlSize(.small)
            } else {
                VStack(spacing: 4) {
                    Image(systemName: "arrow.down.doc")
                        .font(.system(size: 18, weight: .light))
                    Text((artifact.name as NSString).pathExtension.uppercased())
                        .font(.system(size: 9, weight: .semibold))
                }
                .foregroundStyle(Theme.mutedForeground)
            }
        }
        .aspectRatio(1, contentMode: .fit)
        .frame(maxWidth: .infinity)
        .background(Theme.foreground.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Theme.foreground.opacity(0.08))
        )
        .contentShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

/// Small bordered action button, matching the RestartRecommendedBar anatomy.
private struct GalleryActionButton: View {
    let title: String
    let systemImage: String
    var destructive = false
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: systemImage)
                    .font(.system(size: 10, weight: .semibold))
                Text(title)
                    .font(.system(size: 11, weight: .semibold))
            }
            .foregroundStyle(destructive ? Color.red.opacity(0.9) : Theme.foreground)
            .padding(.horizontal, 9)
            .frame(height: 24)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Theme.foreground.opacity(hovering ? 0.13 : 0.08))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(Theme.foreground.opacity(0.10))
            )
            .contentShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityLabel(title)
    }
}
