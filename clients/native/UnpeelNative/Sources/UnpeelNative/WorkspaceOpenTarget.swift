//
//  WorkspaceOpenTarget.swift
//  UnpeelNative
//
//  Targets for the titlebar "open workspace" menu. The actual launching
//  happens in UnpeelStore so failures can use the app's shared alert path.
//

import AppKit
import SwiftUI

enum WorkspaceOpenTarget: String, CaseIterable, Identifiable, Sendable {
    case vscode
    case cursor
    case zed
    case idea
    case webstorm
    case githubDesktop
    case fork
    case tower
    case sourcetree
    case gitkraken
    case sublimeMerge
    case finder
    case terminal
    case iterm2
    case ghostty
    case warp
    case wezterm
    case kitty
    case alacritty
    case tabby
    case hyper
    case rio
    case wave
    case xcode

    var id: String { rawValue }

    var title: String {
        switch self {
        case .vscode: return "VS Code"
        case .cursor: return "Cursor"
        case .zed: return "Zed"
        case .idea: return "IntelliJ"
        case .webstorm: return "WebStorm"
        case .githubDesktop: return "GitHub Desktop"
        case .fork: return "Fork"
        case .tower: return "Tower"
        case .sourcetree: return "Sourcetree"
        case .gitkraken: return "GitKraken"
        case .sublimeMerge: return "Sublime Merge"
        case .finder: return "Finder"
        case .terminal: return "Terminal"
        case .iterm2: return "iTerm2"
        case .ghostty: return "Ghostty"
        case .warp: return "Warp"
        case .wezterm: return "WezTerm"
        case .kitty: return "kitty"
        case .alacritty: return "Alacritty"
        case .tabby: return "Tabby"
        case .hyper: return "Hyper"
        case .rio: return "Rio"
        case .wave: return "Wave"
        case .xcode: return "Xcode"
        }
    }

    var bundleIdentifiers: [String] {
        switch self {
        case .vscode: return ["com.microsoft.VSCode"]
        case .cursor: return ["com.todesktop.230313mzl4w4u92"]
        case .zed: return ["dev.zed.Zed", "dev.zed.Zed-Preview", "dev.zed.Zed-Nightly", "dev.zed.Zed-Dev"]
        case .idea: return ["com.jetbrains.intellij", "com.jetbrains.intellij.ce"]
        case .webstorm: return ["com.jetbrains.WebStorm"]
        case .githubDesktop: return ["com.github.GitHubClient"]
        case .fork: return ["com.DanPristupov.Fork"]
        case .tower: return ["com.fournova.Tower3", "com.fournova.Tower"]
        case .sourcetree: return ["com.torusknot.SourceTreeNotMAS"]
        case .gitkraken: return ["com.axosoft.gitkraken"]
        case .sublimeMerge: return ["com.sublimemerge"]
        case .finder: return ["com.apple.finder"]
        case .terminal: return ["com.apple.Terminal"]
        case .iterm2: return ["com.googlecode.iterm2"]
        case .ghostty: return ["com.mitchellh.ghostty"]
        case .warp: return ["dev.warp.Warp-Stable", "dev.warp.Warp"]
        case .wezterm: return ["com.github.wez.wezterm"]
        case .kitty: return ["net.kovidgoyal.kitty"]
        case .alacritty: return ["org.alacritty"]
        case .tabby: return ["org.tabby"]
        case .hyper: return ["co.zeit.hyper"]
        case .rio: return ["com.raphaelamorim.rio"]
        case .wave: return ["dev.commandline.waveterm"]
        case .xcode: return ["com.apple.dt.Xcode"]
        }
    }

    var appNames: [String] {
        switch self {
        case .vscode: return ["Visual Studio Code"]
        case .cursor: return ["Cursor"]
        case .zed: return ["Zed", "Zed Preview", "Zed Nightly"]
        case .idea: return ["IntelliJ IDEA", "IntelliJ IDEA CE"]
        case .webstorm: return ["WebStorm"]
        case .githubDesktop: return ["GitHub Desktop"]
        case .fork: return ["Fork"]
        case .tower: return ["Tower"]
        case .sourcetree: return ["Sourcetree", "SourceTree"]
        case .gitkraken: return ["GitKraken"]
        case .sublimeMerge: return ["Sublime Merge"]
        case .finder: return ["Finder"]
        case .terminal: return ["Terminal"]
        case .iterm2: return ["iTerm", "iTerm2"]
        case .ghostty: return ["Ghostty"]
        case .warp: return ["Warp"]
        case .wezterm: return ["WezTerm"]
        case .kitty: return ["kitty"]
        case .alacritty: return ["Alacritty"]
        case .tabby: return ["Tabby"]
        case .hyper: return ["Hyper"]
        case .rio: return ["Rio", "rio"]
        case .wave: return ["Wave Terminal", "Wave"]
        case .xcode: return ["Xcode"]
        }
    }

    var fallbackSymbol: String {
        switch self {
        case .finder: return "folder"
        case .terminal, .iterm2, .ghostty, .warp, .wezterm, .kitty,
                .alacritty, .tabby, .hyper, .rio, .wave:
            return "terminal"
        case .xcode: return "hammer.fill"
        case .vscode, .cursor, .zed, .idea, .webstorm:
            return "chevron.left.forwardslash.chevron.right"
        case .githubDesktop, .fork, .tower, .sourcetree, .gitkraken,
                .sublimeMerge:
            return "arrow.triangle.branch"
        }
    }

    var appIconPathCandidates: [String] {
        let home = NSHomeDirectory()
        let appPaths = appNames.flatMap { name in
            [
                "/Applications/\(name).app",
                "\(home)/Applications/\(name).app",
                "/System/Applications/\(name).app",
                "/System/Applications/Utilities/\(name).app",
                "/System/Library/CoreServices/\(name).app",
            ]
        }
        return appPaths + explicitIconPaths
    }

    private var explicitIconPaths: [String] {
        switch self {
        case .finder:
            return ["/System/Library/CoreServices/Finder.app"]
        case .terminal:
            return ["/System/Applications/Utilities/Terminal.app"]
        default:
            return []
        }
    }

    static func preferred(forEditor editor: String) -> WorkspaceOpenTarget {
        let id = editor.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return editorCases.first { $0.codeEditorID == id } ?? .vscode
    }

    /// The `codeEditor` id this target maps to (the value persisted as the
    /// default editor). Only the editor-group targets have one; everything
    /// else (git apps, terminals, Finder) returns nil.
    var codeEditorID: String? {
        switch self {
        case .vscode: return "code"
        case .cursor: return "cursor"
        case .zed: return "zed"
        case .idea: return "idea"
        case .webstorm: return "webstorm"
        case .xcode: return "xcode"
        default: return nil
        }
    }

    /// The editor targets, in menu order, used by both the titlebar dropdown
    /// and the Settings "Default editor" picker.
    static var editorTargets: [WorkspaceOpenTarget] { editorCases }

    static let gitAppCases: [WorkspaceOpenTarget] = [
        .githubDesktop,
        .fork,
        .tower,
        .sourcetree,
        .gitkraken,
        .sublimeMerge,
    ]

    private static let editorCases: [WorkspaceOpenTarget] = [
        .vscode,
        .cursor,
        .zed,
        .idea,
        .webstorm,
        .xcode,
    ]

    private static let terminalCases: [WorkspaceOpenTarget] = [
        .terminal,
        .iterm2,
        .ghostty,
        .warp,
        .wezterm,
        .kitty,
        .alacritty,
        .tabby,
        .hyper,
        .rio,
        .wave,
    ]

    @MainActor
    var isAvailable: Bool {
        if let cached = Self.availabilityCache[self] {
            return cached
        }

        let available = resolveAvailability()
        Self.availabilityCache[self] = available
        return available
    }

    /// Installed-app discovery goes through Launch Services and can take
    /// several milliseconds per bundle id. SwiftUI eagerly evaluates context
    /// menu builders while constructing every project row, so resolving this
    /// repeatedly turned an unrelated sidebar publication into a visible
    /// session-switch hitch. Installation availability is effectively static
    /// for one app run; resolve each target once and reuse it everywhere.
    @MainActor
    private func resolveAvailability() -> Bool {
        if self == .finder { return true }

        for bundleID in bundleIdentifiers
        where NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) != nil {
            return true
        }

        for path in appIconPathCandidates
        where FileManager.default.fileExists(atPath: path) {
            return true
        }

        return false
    }

    @MainActor
    private static var availabilityCache: [WorkspaceOpenTarget: Bool] = [:]

    @MainActor
    private static var menuGroupsCache: [[WorkspaceOpenTarget]]?

    @MainActor
    static var availableCases: [WorkspaceOpenTarget] {
        allCases.filter { $0.isAvailable }
    }

    @MainActor
    static func availableMenuGroups() -> [[WorkspaceOpenTarget]] {
        if let menuGroupsCache {
            return menuGroupsCache
        }

        func available(_ targets: [WorkspaceOpenTarget]) -> [WorkspaceOpenTarget] {
            targets.filter { $0.isAvailable }
        }

        let groups = [
            available(editorCases),
            available(gitAppCases),
            available([.finder]),
            available(terminalCases),
        ].filter { !$0.isEmpty }
        menuGroupsCache = groups
        return groups
    }
}

@MainActor
enum WorkspaceAppIconStore {
    private static var cache: [WorkspaceOpenTarget: NSImage?] = [:]

    static func image(for target: WorkspaceOpenTarget) -> NSImage? {
        if let cached = cache[target] { return cached }

        for bundleID in target.bundleIdentifiers {
            if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) {
                let image = NSWorkspace.shared.icon(forFile: url.path)
                image.isTemplate = false
                cache[target] = image
                return image
            }
        }

        for path in target.appIconPathCandidates {
            if FileManager.default.fileExists(atPath: path) {
                let image = NSWorkspace.shared.icon(forFile: path)
                image.isTemplate = false
                cache[target] = image
                return image
            }
        }

        cache[target] = nil
        return nil
    }

    /// 14pt copy for menu items — NSWorkspace icons come back large, and a
    /// fresh NSImage per menu evaluation would give the items a new identity
    /// each render (the same blink FolderColorMenuSwatch caches against).
    static func menuImage(for target: WorkspaceOpenTarget) -> NSImage? {
        if let cached = menuCache[target] { return cached }
        let resized = image(for: target).flatMap { base -> NSImage? in
            guard let copy = base.copy() as? NSImage else { return nil }
            copy.size = NSSize(width: 14, height: 14)
            return copy
        }
        menuCache[target] = resized
        return resized
    }

    private static var menuCache: [WorkspaceOpenTarget: NSImage?] = [:]
}

struct WorkspaceAppIconView: View {
    let target: WorkspaceOpenTarget
    var size: CGFloat = 16

    var body: some View {
        if let image = WorkspaceAppIconStore.image(for: target) {
            Image(nsImage: image)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .frame(width: size, height: size)
        } else {
            Image(systemName: target.fallbackSymbol)
                .font(.system(size: size * 0.82, weight: .semibold))
                .foregroundStyle(Theme.mutedForeground)
                .frame(width: size, height: size)
        }
    }
}
