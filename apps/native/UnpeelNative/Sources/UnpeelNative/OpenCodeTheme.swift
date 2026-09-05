//
//  OpenCodeTheme.swift
//  UnpeelNative
//
//  Resolves agent TUI themes enough for Unpeel's terminal frame. OpenCode and
//  Grok paint most of their UI with truecolor escape sequences, but the
//  surrounding Ghostty padding/titlebar background is still owned by Unpeel.
//

import AppKit
import Foundation

struct TerminalFrameStyle {
    struct Background: Equatable, Sendable {
        var light: UInt32?
        var dark: UInt32?

        var isEmpty: Bool { light == nil && dark == nil }

        /// Stable key for cache invalidation when provider config changes.
        var signature: String {
            let lightPart = light.map { String(format: "%06X", $0) } ?? "-"
            let darkPart = dark.map { String(format: "%06X", $0) } ?? "-"
            return "\(lightPart)/\(darkPart)"
        }
    }

    let backgroundColor: NSColor
    let paneStyle: TerminalPaneStyle
    /// Provider-resolved background, if any. Nil means Unpeel's default.
    let background: Background?

    /// True when this session's frame is driven by a provider TUI theme
    /// (OpenCode / Grok) rather than Unpeel's default terminal surface.
    var usesProviderTheme: Bool { background != nil }

    static func resolved(for session: SessionEntry, workingDirectory: String?) -> TerminalFrameStyle {
        resolved(command: session.presentationCommand, workingDirectory: workingDirectory)
    }

    static func resolved(command: String, workingDirectory: String?) -> TerminalFrameStyle {
        let background = providerBackground(command: command, workingDirectory: workingDirectory)
        return resolved(command: command, background: background, canvasOverride: nil)
    }

    /// File reads are independent of AppKit. The cache calls this on its
    /// worker queue, then resolves the native colors on the main actor.
    static func providerBackground(command: String, workingDirectory: String?) -> Background? {
        let background: Background?
        switch SetupTool.detect(in: command) {
        case .opencode:
            background = OpenCodeThemeResolver.background(workingDirectory: workingDirectory)
        case .grok:
            background = GrokThemeResolver.background(command: command)
        default:
            background = nil
        }

        return background
    }

    static func resolved(
        command: String, background: Background?, canvasOverride: UInt32?
    ) -> TerminalFrameStyle {
        var paneStyle = TerminalPaneStyle.resolved(command: command)
        let background = canvasOverride.map { Background(light: $0, dark: $0) } ?? background
        guard let background else {
            return TerminalFrameStyle(
                backgroundColor: Theme.terminalBackgroundNSColor,
                paneStyle: paneStyle,
                background: nil
            )
        }

        if let dark = background.dark {
            paneStyle.dark.background = Self.hexString(dark)
        }
        if let light = background.light {
            paneStyle.light.background = Self.hexString(light)
        }

        return TerminalFrameStyle(
            backgroundColor: Self.dynamicColor(for: background),
            paneStyle: paneStyle,
            background: background
        )
    }

    private static func dynamicColor(for background: Background) -> NSColor {
        switch (background.light, background.dark) {
        case let (light?, dark?) where light == dark:
            // Single canvas color (live sample or fixed theme): use a plain
            // sRGB NSColor so SwiftUI titlebar, AppKit layers, and Ghostty's
            // #RRGGBB all resolve to the same pixel values. A dynamic pair
            // with identical ends still went through appearance conversion
            // and left a 1px top/bottom hairline against the Metal surface.
            return NSColor(hex: light)
        case let (light?, dark?):
            return Theme.dynamicNSColor(light: NSColor(hex: light), dark: NSColor(hex: dark))
        case let (light?, nil):
            return Theme.dynamicNSColor(
                light: NSColor(hex: light),
                dark: NSColor(hex: Theme.darkSurfaceHex)
            )
        case let (nil, dark?):
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0xFFFFFF),
                dark: NSColor(hex: dark)
            )
        default:
            return Theme.terminalBackgroundNSColor
        }
    }

    private static func hexString(_ value: UInt32) -> String {
        String(format: "#%06X", value)
    }

    /// The resolved DARK terminal background (0xRRGGBB) for this session's
    /// provider TUI, or nil for the default. Sent to the phone
    /// (`RemoteSessionSummary.terminalBackgroundHex`) so its terminal chrome
    /// can match — prefers the live canvas sample from `output.bin` (what the
    /// TUI is actually painting) over config files, so in-TUI theme changes
    /// show up without a session switch.
    static func darkBackgroundHex(for session: SessionEntry, workingDirectory: String?) -> Int? {
        if usesProviderTheme(command: session.presentationCommand),
           let sampled = ProviderCanvasSampler.dominantBackground(sessionID: session.id) {
            return Int(sampled)
        }
        return resolved(for: session, workingDirectory: workingDirectory).background?.dark.map { Int($0) }
    }

    /// True when this command head is themed by a provider config Unpeel
    /// mirrors into the terminal frame (OpenCode / Grok).
    static func usesProviderTheme(command: String) -> Bool {
        switch SetupTool.detect(in: command) {
        case .opencode, .grok: return true
        default: return false
        }
    }

    /// Frame style for a session, with an optional live canvas override
    /// (0xRRGGBB) sampled from the TUI's truecolor paint.
    static func resolved(
        command: String,
        workingDirectory: String?,
        canvasOverride: UInt32?
    ) -> TerminalFrameStyle {
        let style = resolved(command: command, workingDirectory: workingDirectory)
        guard let canvasOverride else { return style }
        return style.withFixedBackground(canvasOverride)
    }

    /// Force both light/dark frame + Ghostty backgrounds to a single canvas
    /// color (what the TUI is painting right now).
    func withFixedBackground(_ hex: UInt32) -> TerminalFrameStyle {
        var pane = paneStyle
        let hexString = String(format: "#%06X", hex)
        pane.dark.background = hexString
        pane.light.background = hexString
        let background = Background(light: hex, dark: hex)
        return TerminalFrameStyle(
            backgroundColor: NSColor(hex: hex),
            paneStyle: pane,
            background: background
        )
    }
}

/// Reads recent terminal output for the dominant truecolor background the
/// agent TUI is painting. OpenCode/Grok fill the canvas with SGR `48;2;R;G;B`
/// — that is the ground truth for Unpeel's titlebar / Ghostty default bg,
/// even when the provider has not written its theme choice to config yet.
enum ProviderCanvasSampler {
    /// Bytes of the output tail to scan.
    private static let sampleBytes = 96 * 1024
    /// Minimum hits before we trust a color (avoids flash frames).
    private static let minHits = 40
    /// Top color must beat the runner-up by this ratio (canvas vs chrome).
    private static let dominanceRatio = 1.6

    /// Sample memo keyed by output.bin (mtime, size): while the file hasn't
    /// grown there is nothing new to scan — the SurfaceCache poll otherwise
    /// re-read and byte-scanned the 96KB tail per pane ~3×/s on the main
    /// thread, and the phone bootstrap did the same per themed session.
    private struct SampleStamp: Equatable {
        var mtimeSec: Int
        var mtimeNsec: Int
        var size: Int64
    }
    private nonisolated(unsafe) static var sampleCache: [String: (stamp: SampleStamp, sample: UInt32?)] = [:]
    private static let sampleCacheLock = NSLock()

    /// Presentation can reuse the last color without touching disk while a
    /// newly visible pane's fresh sample is being collected in the background.
    static func cachedBackground(sessionID: String, sessionsDir: URL) -> UInt32? {
        let key = sessionsDir.appendingPathComponent(sessionID)
            .appendingPathComponent("output.bin").standardizedFileURL.path
        return sampleCacheLock.withLock { sampleCache[key]?.sample }
    }

    static func dominantBackground(
        sessionID: String,
        sessionsDir: URL = LaunchConfig.appSessionsDir
    ) -> UInt32? {
        let url = sessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent("output.bin")
        let cacheKey = url.standardizedFileURL.path
        var st = stat()
        guard stat(url.path, &st) == 0 else {
            sampleCacheLock.withLock { sampleCache[cacheKey] = nil }
            return nil
        }
        let stamp = SampleStamp(
            mtimeSec: Int(st.st_mtimespec.tv_sec),
            mtimeNsec: Int(st.st_mtimespec.tv_nsec),
            size: Int64(st.st_size)
        )
        let cached = sampleCacheLock.withLock { sampleCache[cacheKey] }
        if let cached, cached.stamp == stamp {
            return cached.sample
        }
        let sample = dominantBackground(inFileAt: url)
        sampleCacheLock.withLock {
            // Tiny entries, but sessions come and go over a long run — reset
            // rather than track lifecycles.
            if sampleCache.count > 128 { sampleCache.removeAll(keepingCapacity: true) }
            sampleCache[cacheKey] = (stamp, sample)
        }
        return sample
    }

    static func dominantBackground(inFileAt url: URL) -> UInt32? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }

        let size: UInt64
        do {
            size = try handle.seekToEnd()
        } catch {
            return nil
        }
        guard size > 0 else { return nil }

        let start = size > UInt64(sampleBytes) ? size - UInt64(sampleBytes) : 0
        do {
            try handle.seek(toOffset: start)
        } catch {
            return nil
        }
        guard let data = try? handle.read(upToCount: sampleBytes), !data.isEmpty else { return nil }
        return dominantBackground(in: data)
    }

    /// Exposed for tests.
    static func dominantBackground(in data: Data) -> UInt32? {
        // Match CSI … 48;2;R;G;B … m  (truecolor background). Also accepts
        // the compound SGR form used by Grok: 38;2;…;48;2;R;G;B.
        var counts: [UInt32: Int] = [:]
        var i = data.startIndex
        let end = data.endIndex
        while i < end {
            // ESC [
            if data[i] == 0x1B {
                let next = data.index(after: i)
                if next < end, data[next] == 0x5B {
                    var j = data.index(after: next)
                    let seqStart = j
                    while j < end {
                        let b = data[j]
                        if (0x40 ... 0x7E).contains(b) {
                            if b == 0x6D { // 'm' SGR
                                tallyTruecolorBackgrounds(
                                    data[seqStart ..< j],
                                    into: &counts
                                )
                            }
                            i = data.index(after: j)
                            break
                        }
                        j = data.index(after: j)
                    }
                    if j >= end { break }
                    continue
                }
            }
            i = data.index(after: i)
        }

        guard let top = counts.max(by: { $0.value < $1.value }) else { return nil }
        guard top.value >= minHits else { return nil }
        let second = counts
            .filter { $0.key != top.key }
            .map(\.value)
            .max() ?? 0
        if second > 0, Double(top.value) < Double(second) * dominanceRatio {
            return nil
        }
        return top.key
    }

    private static func tallyTruecolorBackgrounds(
        _ params: Data,
        into counts: inout [UInt32: Int]
    ) {
        // Parse semicolon-separated integers without allocating strings.
        var numbers: [Int] = []
        var current = 0
        var hasDigit = false
        for byte in params {
            if byte == 0x3B { // ';'
                numbers.append(hasDigit ? current : 0)
                current = 0
                hasDigit = false
            } else if (0x30 ... 0x39).contains(byte) {
                current = current * 10 + Int(byte - 0x30)
                hasDigit = true
                if current > 255_000 { return } // garbage / overflow guard
            } else if byte == 0x3A { // ':' (ISO intermediate) — treat as separator
                numbers.append(hasDigit ? current : 0)
                current = 0
                hasDigit = false
            } else {
                // Unknown char in SGR — abort this sequence
                return
            }
        }
        if hasDigit { numbers.append(current) }

        var index = 0
        while index < numbers.count {
            // 48;2;R;G;B  or  48:2:R:G:B (we already flattened ':' to entries)
            if numbers[index] == 48,
               index + 4 < numbers.count,
               numbers[index + 1] == 2 {
                let r = numbers[index + 2]
                let g = numbers[index + 3]
                let b = numbers[index + 4]
                if (0 ... 255).contains(r), (0 ... 255).contains(g), (0 ... 255).contains(b) {
                    let hex = (UInt32(r) << 16) | (UInt32(g) << 8) | UInt32(b)
                    counts[hex, default: 0] += 1
                }
                index += 5
                continue
            }
            index += 1
        }
    }
}

/// FSEvents roots + path filter for live OpenCode / Grok theme reloads.
enum ProviderThemeWatchPaths {
    /// Directories/files that may hold provider theme config. Only existing
    /// paths are returned (FSEvents requires them).
    static func roots(workingDirectories: [String]) -> [String] {
        var urls: [URL] = []
        var seen = Set<String>()

        func append(_ url: URL) {
            let path = url.standardizedFileURL.path
            guard !seen.contains(path),
                  FileManager.default.fileExists(atPath: path)
            else { return }
            seen.insert(path)
            urls.append(url)
        }

        append(GrokThemeResolver.grokHomeDirectory())
        append(OpenCodeThemeResolver.userConfigDirectory())
        append(OpenCodeThemeResolver.unpeelOpenCodeConfigDirectory())
        append(OpenCodeThemeResolver.stateKVFile().deletingLastPathComponent())

        if let envConfig = ProcessInfo.processInfo.environment["OPENCODE_TUI_CONFIG"],
           !envConfig.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            append(URL(fileURLWithPath: envConfig))
        }

        for raw in workingDirectories {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { continue }
            let directory = URL(fileURLWithPath: trimmed).standardizedFileURL
            append(directory)
            append(directory.appendingPathComponent(".opencode"))
        }

        return urls.map(\.path)
    }

    /// Whether an FSEvents path should trigger a theme re-resolve.
    static func isRelevantChange(_ path: String) -> Bool {
        let url = URL(fileURLWithPath: path)
        let name = url.lastPathComponent.lowercased()
        let parent = url.deletingLastPathComponent().lastPathComponent.lowercased()
        let full = path.lowercased()

        // Grok UI theme lives only in config.toml under GROK_HOME.
        if name == "config.toml", full.contains("/.grok") || full.contains("/grok-home/") {
            return true
        }

        // OpenCode config + theme JSON/JSONC (including project-local).
        if name == "opencode.json" || name == "opencode.jsonc"
            || name == "tui.json" || name == "tui.jsonc"
            || name == "kv.json" {
            return true
        }
        if parent == "themes",
           name.hasSuffix(".json") || name.hasSuffix(".jsonc") {
            return true
        }
        if full.contains("/.config/opencode/") || full.contains("/.opencode/")
            || full.contains("/hooks/opencode/") {
            if name.hasSuffix(".json") || name.hasSuffix(".jsonc") {
                return true
            }
        }
        return false
    }
}

enum OpenCodeThemeResolver {
    static func background(workingDirectory: String?) -> TerminalFrameStyle.Background? {
        let theme = selectedThemeName(workingDirectory: workingDirectory) ?? "opencode"
        let normalized = normalizeThemeName(theme)
        guard normalized != "system", normalized != "transparent" else { return nil }

        if let custom = customThemeBackground(
            themeName: theme,
            normalizedThemeName: normalized,
            workingDirectory: workingDirectory
        ), !custom.isEmpty {
            return custom
        }

        return builtInBackgrounds[normalized]
    }

    /// Mirrors opencode TUI `theme.background`, not `backgroundPanel`.
    private static let builtInBackgrounds: [String: TerminalFrameStyle.Background] = [
        "opencode": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x0A0A0A),
        "oc-1": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x0A0A0A),
        "aura": TerminalFrameStyle.Background(light: 0x0F0F0F, dark: 0x0F0F0F),
        "ayu": TerminalFrameStyle.Background(light: 0x0B0E14, dark: 0x0B0E14),
        "carbonfox": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x161616),
        "catppuccin": TerminalFrameStyle.Background(light: 0xEFF1F5, dark: 0x1E1E2E),
        "catppuccin-frappe": TerminalFrameStyle.Background(light: 0x303446, dark: 0x303446),
        "catppuccin-macchiato": TerminalFrameStyle.Background(light: 0x24273A, dark: 0x24273A),
        "catppuccin-mocha": TerminalFrameStyle.Background(light: 0xEFF1F5, dark: 0x1E1E2E),
        "cobalt2": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x193549),
        "cursor": TerminalFrameStyle.Background(light: 0xFCFCFC, dark: 0x181818),
        "dracula": TerminalFrameStyle.Background(light: 0xF8F8F2, dark: 0x282A36),
        "everforest": TerminalFrameStyle.Background(light: 0xFDF6E3, dark: 0x2D353B),
        "flexoki": TerminalFrameStyle.Background(light: 0xFFFCF0, dark: 0x100F0F),
        "github": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x0D1117),
        "gruvbox": TerminalFrameStyle.Background(light: 0xFBF1C7, dark: 0x282828),
        "kanagawa": TerminalFrameStyle.Background(light: 0xF2E9DE, dark: 0x1F1F28),
        "material": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x263238),
        "matrix": TerminalFrameStyle.Background(light: 0xEEF3EA, dark: 0x0A0E0A),
        "mercury": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x171721),
        "monokai": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x272822),
        "night-owl": TerminalFrameStyle.Background(light: 0x011627, dark: 0x011627),
        "nightowl": TerminalFrameStyle.Background(light: 0x011627, dark: 0x011627),
        "nord": TerminalFrameStyle.Background(light: 0xECEFF4, dark: 0x2E3440),
        "one-dark": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x282C34),
        "one-dark-pro": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x282C34),
        "onedarkpro": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x282C34),
        "orng": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x0A0A0A),
        "osaka-jade": TerminalFrameStyle.Background(light: 0xF6F5DD, dark: 0x111C18),
        "palenight": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x292D3E),
        "rosepine": TerminalFrameStyle.Background(light: 0xFAF4ED, dark: 0x191724),
        "rose-pine": TerminalFrameStyle.Background(light: 0xFAF4ED, dark: 0x191724),
        "shadesofpurple": TerminalFrameStyle.Background(light: 0xF7EBFF, dark: 0x1A102B),
        "shades-of-purple": TerminalFrameStyle.Background(light: 0xF7EBFF, dark: 0x1A102B),
        "solarized": TerminalFrameStyle.Background(light: 0xFDF6E3, dark: 0x002B36),
        "synthwave84": TerminalFrameStyle.Background(light: 0xFAFAFA, dark: 0x262335),
        "tokyonight": TerminalFrameStyle.Background(light: 0xE1E2E7, dark: 0x1A1B26),
        "tokyo-night": TerminalFrameStyle.Background(light: 0xE1E2E7, dark: 0x1A1B26),
        "vercel": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x000000),
        "vesper": TerminalFrameStyle.Background(light: 0xFFFFFF, dark: 0x101010),
        "zenburn": TerminalFrameStyle.Background(light: 0xFFFFEF, dark: 0x3F3F3F),
    ]

    private static func selectedThemeName(workingDirectory: String?) -> String? {
        var selected: String?
        for file in configCandidates(workingDirectory: workingDirectory) {
            guard let object = readJSONObject(at: file) else { continue }
            if let theme = themeName(in: object) {
                selected = theme
            }
        }
        return selected
    }

    private static func themeName(in object: [String: Any]) -> String? {
        if let theme = nonEmptyString(object["theme"]) {
            return theme
        }
        if let tui = object["tui"] as? [String: Any],
           let theme = nonEmptyString(tui["theme"]) {
            return theme
        }
        return nil
    }

    private static func customThemeBackground(
        themeName: String,
        normalizedThemeName: String,
        workingDirectory: String?
    ) -> TerminalFrameStyle.Background? {
        var resolved: TerminalFrameStyle.Background?
        for file in themeCandidates(
            themeName: themeName,
            normalizedThemeName: normalizedThemeName,
            workingDirectory: workingDirectory
        ) {
            guard let object = readJSONObject(at: file),
                  let background = themeBackground(in: object),
                  !background.isEmpty
            else { continue }
            resolved = background
        }
        return resolved
    }

    private static func themeBackground(in object: [String: Any]) -> TerminalFrameStyle.Background? {
        let defs = object["defs"] as? [String: Any] ?? [:]

        if let theme = object["theme"] as? [String: Any],
           let background = background(from: theme["background"], defs: defs) {
            return background
        }

        let light = paletteNeutral(in: object["light"])
        let dark = paletteNeutral(in: object["dark"])
        if light != nil || dark != nil {
            return TerminalFrameStyle.Background(light: light, dark: dark)
        }

        return nil
    }

    private static func background(from value: Any?, defs: [String: Any]) -> TerminalFrameStyle.Background? {
        if let object = value as? [String: Any] {
            let light = resolveHex(object["light"], defs: defs)
                ?? resolveHex(object["default"], defs: defs)
            let dark = resolveHex(object["dark"], defs: defs)
                ?? resolveHex(object["default"], defs: defs)
            return TerminalFrameStyle.Background(light: light, dark: dark)
        }

        if let hex = resolveHex(value, defs: defs) {
            return TerminalFrameStyle.Background(light: hex, dark: hex)
        }

        return nil
    }

    private static func paletteNeutral(in value: Any?) -> UInt32? {
        guard let object = value as? [String: Any],
              let palette = object["palette"] as? [String: Any]
        else { return nil }
        return resolveHex(palette["neutral"], defs: [:])
    }

    private static func resolveHex(
        _ value: Any?,
        defs: [String: Any],
        depth: Int = 0
    ) -> UInt32? {
        guard depth < 8 else { return nil }

        if let object = value as? [String: Any] {
            return resolveHex(object["dark"], defs: defs, depth: depth + 1)
                ?? resolveHex(object["light"], defs: defs, depth: depth + 1)
                ?? resolveHex(object["default"], defs: defs, depth: depth + 1)
        }

        guard let raw = nonEmptyString(value) else { return nil }
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard value.lowercased() != "none" else { return nil }

        if let hex = parseHexColor(value) {
            return hex
        }
        if let referenced = defs[value] {
            return resolveHex(referenced, defs: defs, depth: depth + 1)
        }
        return nil
    }

    private static func configCandidates(workingDirectory: String?) -> [URL] {
        var urls: [URL] = []
        var seen = Set<String>()

        appendConfigFiles(in: userConfigDirectory(), to: &urls, seen: &seen)
        appendConfigFiles(in: unpeelOpenCodeConfigDirectory(), to: &urls, seen: &seen)
        append(stateKVFile(), to: &urls, seen: &seen)

        if let envConfig = nonEmptyString(ProcessInfo.processInfo.environment["OPENCODE_TUI_CONFIG"]) {
            append(URL(fileURLWithPath: envConfig), to: &urls, seen: &seen)
        }

        for directory in ancestorDirectories(workingDirectory: workingDirectory) {
            appendConfigFiles(in: directory, to: &urls, seen: &seen)
            appendConfigFiles(in: directory.appendingPathComponent(".opencode"), to: &urls, seen: &seen)
        }

        return urls
    }

    private static func themeCandidates(
        themeName: String,
        normalizedThemeName: String,
        workingDirectory: String?
    ) -> [URL] {
        var urls: [URL] = []
        var seen = Set<String>()
        let names = unique([themeName, normalizedThemeName])

        appendThemeFiles(names: names, in: userConfigDirectory(), to: &urls, seen: &seen)
        appendThemeFiles(names: names, in: unpeelOpenCodeConfigDirectory(), to: &urls, seen: &seen)

        for directory in ancestorDirectories(workingDirectory: workingDirectory) {
            appendThemeFiles(
                names: names,
                in: directory.appendingPathComponent(".opencode"),
                to: &urls,
                seen: &seen
            )
        }

        return urls
    }

    private static func appendConfigFiles(in directory: URL, to urls: inout [URL], seen: inout Set<String>) {
        appendJSONVariants(named: "opencode", in: directory, to: &urls, seen: &seen)
        appendJSONVariants(named: "tui", in: directory, to: &urls, seen: &seen)
    }

    private static func appendThemeFiles(
        names: [String],
        in directory: URL,
        to urls: inout [URL],
        seen: inout Set<String>
    ) {
        let themesDirectory = directory.appendingPathComponent("themes")
        for name in names {
            appendJSONVariants(named: name, in: themesDirectory, to: &urls, seen: &seen)
        }
    }

    private static func appendJSONVariants(
        named baseName: String,
        in directory: URL,
        to urls: inout [URL],
        seen: inout Set<String>
    ) {
        append(directory.appendingPathComponent("\(baseName).json"), to: &urls, seen: &seen)
        append(directory.appendingPathComponent("\(baseName).jsonc"), to: &urls, seen: &seen)
    }

    private static func append(_ url: URL, to urls: inout [URL], seen: inout Set<String>) {
        let path = url.standardizedFileURL.path
        guard !seen.contains(path), FileManager.default.fileExists(atPath: path) else { return }
        seen.insert(path)
        urls.append(url)
    }

    static func userConfigDirectory() -> URL {
        if let xdg = nonEmptyString(ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"]) {
            return URL(fileURLWithPath: xdg).appendingPathComponent("opencode")
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config")
            .appendingPathComponent("opencode")
    }

    static func unpeelOpenCodeConfigDirectory() -> URL {
        // Prefer the active workspace home when UNPEEL_HOME is set, matching
        // hook install paths for blank/workspace instances.
        if let home = nonEmptyString(ProcessInfo.processInfo.environment["UNPEEL_HOME"]) {
            return URL(fileURLWithPath: home)
                .appendingPathComponent("hooks")
                .appendingPathComponent("opencode")
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".unpeel")
            .appendingPathComponent("hooks")
            .appendingPathComponent("opencode")
    }

    static func stateKVFile() -> URL {
        let stateHome: URL
        if let xdg = nonEmptyString(ProcessInfo.processInfo.environment["XDG_STATE_HOME"]) {
            stateHome = URL(fileURLWithPath: xdg)
        } else {
            stateHome = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".local")
                .appendingPathComponent("state")
        }
        return stateHome
            .appendingPathComponent("opencode")
            .appendingPathComponent("kv.json")
    }

    private static func ancestorDirectories(workingDirectory: String?) -> [URL] {
        guard let workingDirectory = nonEmptyString(workingDirectory) else { return [] }

        var current = URL(fileURLWithPath: workingDirectory).standardizedFileURL
        var result: [URL] = []
        var seen = Set<String>()

        while true {
            let path = current.path
            if seen.contains(path) { break }
            seen.insert(path)
            result.append(current)

            let parent = current.deletingLastPathComponent()
            if parent.path == current.path { break }
            current = parent
        }

        return result.reversed()
    }

    private static func readJSONObject(at url: URL) -> [String: Any]? {
        guard let data = try? Data(contentsOf: url),
              let string = String(data: data, encoding: .utf8)
        else { return nil }

        let stripped = stripTrailingCommas(stripJSONCComments(string))
        guard let jsonData = stripped.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: jsonData) as? [String: Any]
        else { return nil }
        return object
    }

    private static func stripJSONCComments(_ input: String) -> String {
        let chars = Array(input)
        var output = ""
        var index = 0
        var inString = false
        var escaping = false
        var inLineComment = false
        var inBlockComment = false

        while index < chars.count {
            let char = chars[index]
            let next = index + 1 < chars.count ? chars[index + 1] : "\0"

            if inLineComment {
                if char == "\n" {
                    inLineComment = false
                    output.append(char)
                }
                index += 1
                continue
            }

            if inBlockComment {
                if char == "*" && next == "/" {
                    inBlockComment = false
                    index += 2
                } else {
                    index += 1
                }
                continue
            }

            if inString {
                output.append(char)
                if escaping {
                    escaping = false
                } else if char == "\\" {
                    escaping = true
                } else if char == "\"" {
                    inString = false
                }
                index += 1
                continue
            }

            if char == "\"" {
                inString = true
                output.append(char)
                index += 1
            } else if char == "/" && next == "/" {
                inLineComment = true
                index += 2
            } else if char == "/" && next == "*" {
                inBlockComment = true
                index += 2
            } else {
                output.append(char)
                index += 1
            }
        }

        return output
    }

    private static func stripTrailingCommas(_ input: String) -> String {
        let chars = Array(input)
        var output = ""
        var index = 0
        var inString = false
        var escaping = false

        while index < chars.count {
            let char = chars[index]

            if inString {
                output.append(char)
                if escaping {
                    escaping = false
                } else if char == "\\" {
                    escaping = true
                } else if char == "\"" {
                    inString = false
                }
                index += 1
                continue
            }

            if char == "\"" {
                inString = true
                output.append(char)
                index += 1
                continue
            }

            if char == "," {
                var lookahead = index + 1
                while lookahead < chars.count, chars[lookahead].isWhitespace {
                    lookahead += 1
                }
                if lookahead < chars.count, chars[lookahead] == "}" || chars[lookahead] == "]" {
                    index += 1
                    continue
                }
            }

            output.append(char)
            index += 1
        }

        return output
    }

    private static func parseHexColor(_ value: String) -> UInt32? {
        guard value.hasPrefix("#") else { return nil }
        let body = String(value.dropFirst())
        let expanded: String
        if body.count == 3 {
            expanded = body.map { "\($0)\($0)" }.joined()
        } else {
            expanded = body
        }
        guard expanded.count == 6,
              expanded.allSatisfy(\.isHexDigit)
        else { return nil }
        return UInt32(expanded, radix: 16)
    }

    private static func normalizeThemeName(_ value: String) -> String {
        value
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .replacingOccurrences(of: "_", with: "-")
    }

    private static func nonEmptyString(_ value: Any?) -> String? {
        guard let string = value as? String else { return nil }
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private static func unique(_ values: [String]) -> [String] {
        var result: [String] = []
        var seen = Set<String>()
        for value in values {
            let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !normalized.isEmpty, !seen.contains(normalized) else { continue }
            seen.insert(normalized)
            result.append(normalized)
        }
        return result
    }
}

enum GrokThemeResolver {
    private struct UIConfig {
        var theme: String?
        var autoLightTheme: String?
        var autoDarkTheme: String?
    }

    static func background(command: String) -> TerminalFrameStyle.Background? {
        if commandHasLightFlag(command) {
            return fixedBackground(for: "grokday")
        }

        let config = readUIConfig()
        let selected = normalizeThemeName(config.theme ?? "auto")
        if selected == "auto" || selected == "system" {
            return autoBackground(config: config)
        }

        return fixedBackground(for: selected) ?? autoBackground(config: config)
    }

    private static func autoBackground(config: UIConfig) -> TerminalFrameStyle.Background {
        let light = themeBackground(
            for: config.autoLightTheme,
            fallback: "grokday"
        )
        let dark = themeBackground(
            for: config.autoDarkTheme,
            fallback: "groknight"
        )
        return TerminalFrameStyle.Background(light: light, dark: dark)
    }

    private static func fixedBackground(for themeName: String) -> TerminalFrameStyle.Background? {
        guard let color = themeBackgrounds[normalizeThemeName(themeName)] else { return nil }
        return TerminalFrameStyle.Background(light: color, dark: color)
    }

    private static func themeBackground(for themeName: String?, fallback: String) -> UInt32 {
        if let themeName,
           let color = themeBackgrounds[normalizeThemeName(themeName)] {
            return color
        }
        return themeBackgrounds[fallback] ?? Theme.darkSurfaceHex
    }

    /// Grok paints its canvas with truecolor SGR; these match the current
    /// CLI canvas colors so Unpeel's titlebar / Ghostty default background
    /// line up with the TUI (sampled from live `output.bin`, grok 0.2.x).
    private static let themeBackgrounds: [String: UInt32] = [
        // Default night family — Grok's truecolor canvas is rgb(20,20,20).
        "groknight": 0x141414,
        "grok-night": 0x141414,
        "dark": 0x141414,
        "grokday": 0xFAFAFA,
        "grok-day": 0xFAFAFA,
        "light": 0xFAFAFA,
        "day": 0xFAFAFA,
        "tokyonight": 0x1A1B26,
        "tokyo-night": 0x1A1B26,
        "tokyo": 0x1A1B26,
        "rosepine": 0x232136,
        "rose-pine": 0x232136,
        "rosepine-moon": 0x232136,
        "rose-pine-moon": 0x232136,
        "oscura": 0x100D1B,
        "oscura-midnight": 0x100D1B,
    ]

    private static func readUIConfig() -> UIConfig {
        let url = grokHomeDirectory().appendingPathComponent("config.toml")
        guard let contents = try? String(contentsOf: url, encoding: .utf8) else {
            return UIConfig()
        }

        var config = UIConfig()
        var section: String?
        for rawLine in contents.components(separatedBy: .newlines) {
            let line = stripTOMLComment(rawLine)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            guard !line.isEmpty else { continue }

            if line.hasPrefix("[") && line.hasSuffix("]") {
                let name = line
                    .dropFirst()
                    .dropLast()
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                section = name
                continue
            }

            guard section == "ui",
                  let equals = line.firstIndex(of: "=")
            else { continue }

            let key = line[..<equals].trimmingCharacters(in: .whitespacesAndNewlines)
            let value = parseTOMLString(String(line[line.index(after: equals)...]))
            switch key {
            case "theme":
                config.theme = value
            case "auto_light_theme":
                config.autoLightTheme = value
            case "auto_dark_theme":
                config.autoDarkTheme = value
            default:
                continue
            }
        }

        return config
    }

    private static func stripTOMLComment(_ line: String) -> String {
        var output = ""
        var quote: Character?
        var escaping = false

        for char in line {
            if let activeQuote = quote {
                output.append(char)
                if escaping {
                    escaping = false
                } else if activeQuote == "\"" && char == "\\" {
                    escaping = true
                } else if char == activeQuote {
                    quote = nil
                }
                continue
            }

            if char == "\"" || char == "'" {
                quote = char
                output.append(char)
            } else if char == "#" {
                break
            } else {
                output.append(char)
            }
        }

        return output
    }

    private static func parseTOMLString(_ raw: String) -> String? {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let first = value.first else { return nil }

        if first == "\"" || first == "'" {
            let chars = Array(value)
            var result = ""
            var escaping = false
            for index in 1 ..< chars.count {
                let char = chars[index]
                if escaping {
                    result.append(char)
                    escaping = false
                } else if first == "\"" && char == "\\" {
                    escaping = true
                } else if char == first {
                    return result.trimmingCharacters(in: .whitespacesAndNewlines)
                } else {
                    result.append(char)
                }
            }
            return nil
        }

        let token = value
            .split(whereSeparator: { $0.isWhitespace || $0 == "," })
            .first
            .map(String.init)
        return token?.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func grokHomeDirectory() -> URL {
        if let home = nonEmptyString(ProcessInfo.processInfo.environment["GROK_HOME"]) {
            return URL(fileURLWithPath: home)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".grok")
    }

    private static func commandHasLightFlag(_ command: String) -> Bool {
        command
            .split(whereSeparator: \.isWhitespace)
            .contains("--light")
    }

    private static func normalizeThemeName(_ value: String) -> String {
        value
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .replacingOccurrences(of: "_", with: "-")
    }

    private static func nonEmptyString(_ value: Any?) -> String? {
        guard let string = value as? String else { return nil }
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
