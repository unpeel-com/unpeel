//
//  SessionArtifacts.swift
//  UnpeelNative
//
//  Shared on-disk model for a session's gallery artifacts under
//  ~/.unpeel/app-sessions/<id>/artifacts/. One kind list + directory
//  mapping for every gallery surface — the desktop title-bar gallery
//  (SessionGalleryPanel) and the phone's /mobile/artifacts routes
//  (MobileSessionControl) — so the two can never drift.
//

import Foundation

struct SessionArtifact: Identifiable, Equatable, Sendable {
    let kind: String
    let name: String
    let url: URL
    let size: UInt64
    let modifiedAt: Date

    /// Stable within a session: kind + filename is the on-disk address.
    var id: String { "\(kind)/\(name)" }

    var isImage: Bool {
        ["png", "jpg", "jpeg", "gif", "webp"].contains(
            (name as NSString).pathExtension.lowercased()
        )
    }
}

enum SessionArtifactStore {
    /// Gallery kinds. `screenshots` and `downloads` are browser-MCP output
    /// (under `artifacts/browser/`), `computer` is computer-MCP captures,
    /// and `uploads` is images the user added or edited from the phone — so
    /// the gallery is a unified per-session image view, not just one
    /// source's captures.
    static let listedKinds = ["screenshots", "downloads", "uploads", "computer"]

    static func root(_ sessionID: String) -> URL {
        LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID, isDirectory: true)
            .appendingPathComponent("artifacts", isDirectory: true)
    }

    /// Resolve a gallery kind to its on-disk dir. `uploads` sits directly
    /// under `artifacts/`; the browser kinds live under `artifacts/browser/`.
    /// Returns nil for an unknown kind (callers 404 / skip).
    static func kindDir(_ sessionID: String, kind: String) -> URL? {
        let base = root(sessionID)
        switch kind {
        case "screenshots":
            return base.appendingPathComponent("browser/screenshots", isDirectory: true)
        case "downloads":
            return base.appendingPathComponent("browser/downloads", isDirectory: true)
        case "uploads":
            return base.appendingPathComponent("uploads", isDirectory: true)
        case "computer":
            // Computer MCP captures (`see`/`screenshot`) — the Rust server
            // writes here, same contract as the browser kinds.
            return base.appendingPathComponent("computer/screenshots", isDirectory: true)
        default:
            return nil
        }
    }

    /// Downscaled phone-gallery variants live here — a sibling of the kind
    /// dirs, deliberately not a listed kind so it never shows in a gallery.
    static func thumbsDir(_ sessionID: String) -> URL {
        root(sessionID).appendingPathComponent("thumbs", isDirectory: true)
    }

    /// Kinds that are agent captures (browser/computer screenshots) — the
    /// ones whose arrival pulses the gallery button on desktop and phone.
    /// Uploads/downloads are excluded: the user put those there themselves.
    static let captureKinds = ["screenshots", "computer"]

    /// Newest capture mtime (ms epoch) for the pulse signal; 0 when none.
    static func latestCaptureUnixMs(_ sessionID: String) -> Int64 {
        var latest: Int64 = 0
        for kind in captureKinds {
            guard let dir = kindDir(sessionID, kind: kind) else { continue }
            let entries = (try? FileManager.default.contentsOfDirectory(
                at: dir,
                includingPropertiesForKeys: [.contentModificationDateKey, .isRegularFileKey],
                options: [.skipsHiddenFiles]
            )) ?? []
            for url in entries {
                let values = try? url.resourceValues(
                    forKeys: [.contentModificationDateKey, .isRegularFileKey]
                )
                guard values?.isRegularFile == true,
                      let date = values?.contentModificationDate
                else { continue }
                latest = max(latest, Int64(date.timeIntervalSince1970 * 1000))
            }
        }
        return latest
    }

    /// Every artifact across the listed kinds, newest-first.
    static func list(_ sessionID: String) -> [SessionArtifact] {
        var artifacts: [SessionArtifact] = []
        for kind in listedKinds {
            guard let dir = kindDir(sessionID, kind: kind) else { continue }
            let entries = (try? FileManager.default.contentsOfDirectory(
                at: dir,
                includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey],
                options: [.skipsHiddenFiles]
            )) ?? []
            for url in entries {
                let values = try? url.resourceValues(
                    forKeys: [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey]
                )
                guard values?.isRegularFile == true else { continue }
                artifacts.append(SessionArtifact(
                    kind: kind,
                    name: url.lastPathComponent,
                    url: url,
                    size: UInt64(values?.fileSize ?? 0),
                    modifiedAt: values?.contentModificationDate ?? .distantPast
                ))
            }
        }
        artifacts.sort { $0.modifiedAt > $1.modifiedAt }
        return artifacts
    }

    /// Remove one artifact from disk, plus any cached thumbnail variants.
    /// Idempotent: a missing file is a no-op success.
    static func delete(_ sessionID: String, kind: String, name: String) throws {
        guard let dir = kindDir(sessionID, kind: kind) else { return }
        let url = dir.appendingPathComponent(name)
        if FileManager.default.fileExists(atPath: url.path) {
            try FileManager.default.removeItem(at: url)
        }
        for thumb in thumbnailVariants(sessionID, kind: kind, name: name, keeping: nil) {
            try? FileManager.default.removeItem(at: thumb)
        }
    }

    /// Cached thumbnail files for one artifact, optionally excluding the
    /// freshly-generated variant being kept.
    static func thumbnailVariants(
        _ sessionID: String,
        kind: String,
        name: String,
        keeping: URL?
    ) -> [URL] {
        let suffix = "-\(kind)-\(name).jpg"
        let entries = (try? FileManager.default.contentsOfDirectory(
            at: thumbsDir(sessionID),
            includingPropertiesForKeys: nil
        )) ?? []
        return entries.filter {
            $0.lastPathComponent.hasSuffix(suffix)
                && $0.lastPathComponent != keeping?.lastPathComponent
        }
    }
}
