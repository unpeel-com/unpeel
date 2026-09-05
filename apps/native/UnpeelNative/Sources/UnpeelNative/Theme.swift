//
//  Theme.swift
//  UnpeelNative
//
//  Resolved design tokens from DESIGN.md (extracted from the Svelte app,
//  light + dark themes, default color scheme). No Ghostty imports here.
//
//  Light/dark switching is appearance-driven: tokens are dynamic NSColors
//  (resolved per the view's effective appearance), the window follows
//  NSApp.appearance, and the Ghostty surface flips its own light/dark
//  config via the wrapper's viewDidChangeEffectiveAppearance hook. The
//  user preference lives in ThemePreference (Light/Dark/System).
//

import AppKit
import SwiftUI
import UnpeelShared

extension NSColor {
    /// 0xRRGGBB hex literal (sRGB).
    convenience init(hex: UInt32, opacity: Double = 1) {
        self.init(
            srgbRed: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            alpha: opacity
        )
    }
}

extension Color {
    /// 0xRRGGBB hex literal.
    init(hex: UInt32, opacity: Double = 1) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: opacity
        )
    }

    /// Appearance-dynamic color, the `body[data-theme]` CSS-variable
    /// equivalent: resolves per the view's effective appearance, so the
    /// whole token set flips when the window appearance changes.
    init(light: NSColor, dark: NSColor) {
        self.init(nsColor: Theme.dynamicNSColor(light: light, dark: dark))
    }

    /// Linear sRGB-space mix toward `other` (0 = self, 1 = other), for
    /// finger-scrubbed color blends (the footer dots' gooey blob). Resolves
    /// through NSColor, so appearance-dynamic endpoints blend at their
    /// currently effective values.
    func blended(with other: Color, fraction: Double) -> Color {
        let f = min(1, max(0, fraction))
        guard f > 0 else { return self }
        guard f < 1 else { return other }
        guard
            let from = NSColor(self).usingColorSpace(.sRGB),
            let to = NSColor(other).usingColorSpace(.sRGB),
            let mixed = from.blended(withFraction: f, of: to)
        else { return f < 0.5 ? self : other }
        return Color(nsColor: mixed)
    }
}

// MARK: - Theme preference (the Tauri Appearance tab's `theme` value)

/// Mirrors the Tauri app's theme setting ("light" / "dark" / "system",
/// state.rs default "system"). The native value is a UserDefaults overlay
/// over the (read-only) app-state.json `theme` field, same merge rule as
/// pins and presets: until the user picks a mode natively, Unpeel follows
/// whatever the Tauri app last saved.
enum ThemePreference: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }

    /// The NSAppearance override for NSApp; nil = follow macOS.
    var nsAppearance: NSAppearance? {
        switch self {
        case .system: return nil
        case .light: return NSAppearance(named: .aqua)
        case .dark: return NSAppearance(named: .darkAqua)
        }
    }
}

/// Native-only sidebar folder palette. Stored by raw value in UserDefaults;
/// app-state.json stays owned by the shared backend.
enum ProjectFolderColor: String, CaseIterable, Identifiable {
    case sky
    case blue
    case violet
    case rose
    case amber
    case moss
    case teal
    case graphite

    var id: String { rawValue }

    var title: String {
        switch self {
        case .sky: return "Sky"
        case .blue: return "Blue"
        case .violet: return "Violet"
        case .rose: return "Rose"
        case .amber: return "Amber"
        case .moss: return "Moss"
        case .teal: return "Teal"
        case .graphite: return "Graphite"
        }
    }

    var nsColor: NSColor {
        switch self {
        case .sky:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0x2095C9), dark: NSColor(hex: 0x7DD3FC)
            )
        case .blue:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0x4F73E6), dark: NSColor(hex: 0x7EA6FF)
            )
        case .violet:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0x7B5BDA), dark: NSColor(hex: 0xB79CFF)
            )
        case .rose:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0xD75F8F), dark: NSColor(hex: 0xF79AC0)
            )
        case .amber:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0xB87511), dark: NSColor(hex: 0xF8C86A)
            )
        case .moss:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0x5F9A3D), dark: NSColor(hex: 0x9DD67A)
            )
        case .teal:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0x159B91), dark: NSColor(hex: 0x64DCCB)
            )
        case .graphite:
            return Theme.dynamicNSColor(
                light: NSColor(hex: 0x687083), dark: NSColor(hex: 0xB8BCC8)
            )
        }
    }

    var tint: Color { Color(nsColor: nsColor) }

    /// Stable terminal accent exported to Apps hosted in this project. The
    /// two values mirror `nsColor`, but stay explicit so the Host contract is
    /// an exact sRGB `#RRGGBB` value rather than an appearance-dependent
    /// AppKit color archive.
    func accentHex(isDark: Bool) -> String {
        switch (self, isDark) {
        case (.sky, false): return "#2095C9"
        case (.sky, true): return "#7DD3FC"
        case (.blue, false): return "#4F73E6"
        case (.blue, true): return "#7EA6FF"
        case (.violet, false): return "#7B5BDA"
        case (.violet, true): return "#B79CFF"
        case (.rose, false): return "#D75F8F"
        case (.rose, true): return "#F79AC0"
        case (.amber, false): return "#B87511"
        case (.amber, true): return "#F8C86A"
        case (.moss, false): return "#5F9A3D"
        case (.moss, true): return "#9DD67A"
        case (.teal, false): return "#159B91"
        case (.teal, true): return "#64DCCB"
        case (.graphite, false): return "#687083"
        case (.graphite, true): return "#B8BCC8"
        }
    }
}

/// Workspace-wide chrome tint (Settings ▸ Appearance ▸ App color). The hues
/// mirror the website's agent-accent palette (apps/website/src/style.css
/// `@theme`) so the app's brand colors and the site agree; `none` is the
/// shipped neutral gray chrome. Native-only UserDefaults overlay, so each
/// workspace instance keeps its own tint; paired phones inherit it over the
/// /mobile bootstrap (`hostTintHue`).
enum AppTint: String, CaseIterable, Identifiable {
    case none
    case peel
    case amber
    case green
    case teal
    case blue
    case indigo
    case violet

    var id: String { rawValue }

    var title: String {
        switch self {
        case .none: return "Default"
        case .peel: return "Peel"
        case .amber: return "Amber"
        case .green: return "Green"
        case .teal: return "Teal"
        case .blue: return "Blue"
        case .indigo: return "Indigo"
        case .violet: return "Violet"
        }
    }

    /// Hue (degrees) the neutral chrome is washed toward; nil = no wash.
    /// Sources: peel = --color-agent-claude #D97757, amber = status-busy,
    /// green = agent-green, teal = agent-codex, blue = agent-kimi #4FA8FF,
    /// indigo = agent-gemini, violet = agent-cursor.
    var hue: Double? {
        switch self {
        case .none: return nil
        case .peel: return 17
        case .amber: return 45
        case .green: return 140
        case .teal: return 187
        case .blue: return 212
        case .indigo: return 243
        case .violet: return 285
        }
    }

    /// Exact website/mascot-family accent exported to hosted terminal Apps.
    /// The neutral workspace leaves this nil so each App retains its own
    /// standalone default unless its project has a folder color.
    var accentHex: String? {
        switch self {
        case .none: return nil
        case .peel: return "#D97757"
        case .amber: return "#E3A63B"
        case .green: return "#3FBF63"
        case .teal: return "#4EC3C9"
        case .blue: return "#4FA8FF"
        case .indigo: return "#7A7EF2"
        case .violet: return "#B166E8"
        }
    }

    /// Representative chip for the settings picker (website accent family).
    var swatch: Color {
        switch self {
        case .none:
            // The neutral default renders as the appearance's full-contrast
            // ink — white chip/dot in dark mode, black in light — rather
            // than a gray that reads as "disabled" next to the accents.
            return Color(
                light: NSColor(hex: 0x1A1A1A), dark: NSColor(hex: 0xFFFFFF)
            )
        case .peel: return Color(hex: 0xD97757)
        case .amber: return Color(hex: 0xE3A63B)
        case .green: return Color(hex: 0x3FBF63)
        case .teal: return Color(hex: 0x4EC3C9)
        case .blue: return Color(hex: 0x4FA8FF)
        case .indigo: return Color(hex: 0x7A7EF2)
        case .violet: return Color(hex: 0xB166E8)
        }
    }
}

extension Notification.Name {
    /// Posted by UnpeelStore.setAppTint after Theme.appTintHue changes, so
    /// non-SwiftUI consumers (SurfaceCache → live Ghostty panes) repaint.
    static let unpeelAppTintChanged = Notification.Name("unpeelAppTintChanged")
    /// Throttled (~10Hz) tick during a tint fade so the VISIBLE terminal
    /// canvases can follow the animating chrome instead of snapping once at
    /// completion. Consumed only by SurfaceCache's visible-panes restyle;
    /// everything expensive still waits for `unpeelAppTintChanged`.
    static let unpeelAppTintAnimationTick =
        Notification.Name("unpeelAppTintAnimationTick")

    /// Posted when ANY workspace's stored color changes (the per-line pickers
    /// in Settings ▸ Workspaces, including other-local and remote overrides),
    /// so the sidebar workspace selector refreshes its dots. Distinct from
    /// `unpeelAppTintChanged`, which only fires when THIS window's chrome hue
    /// moves.
    static let unpeelWorkspaceTintChanged = Notification.Name("unpeelWorkspaceTintChanged")

    /// Posted when this instance renames, creates, or removes a workspace
    /// (Settings ▸ Workspaces or the sidebar picker), so name-derived chrome
    /// — the Settings titlebar's "Settings — <workspace>" — re-reads without
    /// waiting for its next appearance. Cross-instance renames still land on
    /// the next Settings open.
    static let unpeelWorkspaceListChanged = Notification.Name("unpeelWorkspaceListChanged")

    /// Posted (debounced) after Settings ▸ Appearance transparency values
    /// settle, for the expensive listeners: SurfaceCache pushes new Ghostty
    /// background-opacity configs and AppDelegate re-applies the window
    /// blur. SwiftUI chrome follows the published values directly instead.
    static let unpeelTransparencyChanged = Notification.Name("unpeelTransparencyChanged")
}

/// Settings ▸ Appearance window transparency: a window-wide frame backdrop
/// opacity and a terminal canvas opacity. The Background slider is an honest
/// linear scale since 2026-09-01 — the old 90% detent that swapped in the
/// system `.sidebar` material (with its own AppKit-controlled translucency)
/// is gone; every value below 100% is a plain wash at that alpha over the
/// hudWindow glass base. Native-only view preference (like sidebar width),
/// persisted as a UserDefaults overlay, never in `app-state.json`.
@MainActor
final class TransparencyModel: ObservableObject {
    static let shared = TransparencyModel()

    /// Full range, Ghostty-style (2026-08-23): 0% surface makes the terminal
    /// canvas fully invisible so text floats on the window Background, and
    /// 0% Background leaves pure blurred desktop glass. The old 30% floor
    /// ("below reads as broken") was a guardrail users asked to remove.
    static let opacityRange: ClosedRange<Double> = 0.0 ... 1.0
    static let opacityStep: Double = 0.05

    /// The shipped default Background opacity — a real wash, nothing special
    /// about the value (the heavy sidebarTint layer above it keeps the frame
    /// dark; this mostly sets how much blurred desktop bleeds through).
    /// 40% since 2026-09-01; until then the historical 90% position swapped
    /// in the system sidebar material, which is why the name says
    /// "material" — it survives because saved-value plumbing and the remote
    /// reset paths reference it.
    static let backgroundMaterialOpacity: Double = 0.4

    /// Area tones: each area's base brightness on one ABSOLUTE scale. The
    /// design detents mean "use the designed per-appearance colors"; they
    /// sit at the dark design's measured brightness: background #121314 ≈
    /// 8%, surface #1A1B1D ≈ 11%. The tone SLIDERS were removed 2026-09-01
    /// ("good defaults" decision): local suites no longer read saved tone
    /// values, and tones move only when a remote Host's scoped appearance
    /// presents its own (older Hosts may still store custom tones).
    static let designBackgroundTone: Double = 0.08
    static let designSurfaceTone: Double = 0.11
    static let toneRange: ClosedRange<Double> = 0.0 ... 1.0
    static let toneStep: Double = 0.01

    private static let frameKey = "frame_background_opacity"
    /// Pre-rename key ("Sidebar" briefly shipped in dev as its own slider
    /// before becoming the window-wide frame backdrop); read as fallback.
    private static let legacySidebarKey = "sidebar_background_opacity"
    private static let terminalKey = "terminal_background_opacity"
    private static let backgroundToneKey = "background_tone"
    private static let surfaceToneKey = "surface_tone"

    /// One backdrop for the whole window frame — sidebar, the corner strip
    /// at the sidebar/content boundary, and everything behind a translucent
    /// terminal. A single surface means the content pane's rounded corners
    /// can never expose a differently-shaded band.
    @Published var backgroundOpacity: Double {
        didSet { valueChanged() }
    }
    @Published var surfaceOpacity: Double {
        didSet {
            Theme.terminalBackgroundOpacity = surfaceOpacity
            valueChanged()
        }
    }
    @Published var backgroundTone: Double {
        didSet { valueChanged() }
    }
    @Published var surfaceTone: Double {
        didSet {
            Theme.surfaceToneOverride = surfaceUsesDesignTone ? nil : surfaceTone
            valueChanged()
        }
    }

    /// True while the tone slider sits on its design detent — the area keeps
    /// its designed per-appearance colors instead of an absolute gray.
    var backgroundUsesDesignTone: Bool {
        abs(backgroundTone - Self.designBackgroundTone) < 0.001
    }
    var surfaceUsesDesignTone: Bool {
        abs(surfaceTone - Self.designSurfaceTone) < 0.001
    }

    /// True when every value sits on its shipped default (90% Background,
    /// opaque surfaces, design tones).
    var isDefault: Bool {
        abs(backgroundOpacity - Self.backgroundMaterialOpacity) < 0.001
            && surfaceOpacity >= 1
            && backgroundUsesDesignTone
            && surfaceUsesDesignTone
    }

    /// Settings ▸ Appearance "Revert to default": back to the shipped look.
    func resetToDefaults() {
        backgroundOpacity = Self.backgroundMaterialOpacity
        surfaceOpacity = 1
        backgroundTone = Self.designBackgroundTone
        surfaceTone = Self.designSurfaceTone
    }

    /// True when any surface lets the (blurred) desktop show through.
    /// Desktop blur is native: the translucent Background paths stack their
    /// wash over an NSVisualEffectView glass base — never the private CGS
    /// window-blur call, which draws a rim around the window on macOS 26.
    var isTranslucent: Bool {
        surfaceOpacity < 1 || backgroundOpacity < 1
    }

    private var announceWorkItem: DispatchWorkItem?
    /// A remote Host's appearance is presentation state for this Controller,
    /// not a write into this Mac workspace's defaults suite.
    private var suppressPersistence = false

    private init() {
        let values = Self.savedValues(in: AppDefaults.shared)
        backgroundOpacity = values.background
        surfaceOpacity = values.surface
        backgroundTone = values.backgroundTone
        surfaceTone = values.surfaceTone
        Theme.terminalBackgroundOpacity = surfaceOpacity
        Theme.surfaceToneOverride =
            abs(surfaceTone - Self.designSurfaceTone) < 0.001 ? nil : surfaceTone
    }

    /// A workspace's saved transparency values from ITS defaults suite —
    /// the same fallbacks as the live model's init. The scoped Appearance
    /// editor reads and writes another workspace's look through these.
    static func savedValues(
        in defaults: UserDefaults
    ) -> (background: Double, surface: Double, backgroundTone: Double, surfaceTone: Double) {
        // Decision 4 (workspace-scope-and-pairing.md): a workspace with no
        // value of its own inherits the default workspace's baseline from
        // the shared `.standard` domain. For the default instance the given
        // suite IS `.standard`, so the extra hop is a no-op.
        func stored(_ key: String) -> Double? {
            if let value = defaults.object(forKey: key) as? Double { return value }
            return UserDefaults.standard.object(forKey: key) as? Double
        }
        func opacity(_ key: String, default fallback: Double) -> Double {
            guard let value = stored(key) else { return fallback }
            return min(max(value, opacityRange.lowerBound), 1)
        }
        // Tones are always the design detents: the sliders are gone, so any
        // previously saved custom tone (or old design pair) is ignored
        // rather than migrated.
        return (
            opacity(
                frameKey,
                default: opacity(legacySidebarKey, default: backgroundMaterialOpacity)
            ),
            opacity(terminalKey, default: 1),
            designBackgroundTone,
            designSurfaceTone
        )
    }

    /// Whether the suite records ANY transparency value of its own (the
    /// revert button's enablement).
    static func hasSavedValues(in defaults: UserDefaults) -> Bool {
        [frameKey, legacySidebarKey, terminalKey, backgroundToneKey, surfaceToneKey]
            .contains { defaults.object(forKey: $0) != nil }
    }

    /// Decision 4's revert: drop a workspace's own transparency overrides so
    /// it falls back to inheriting the default workspace's baseline.
    static func clearSavedValues(in defaults: UserDefaults) {
        for key in [frameKey, legacySidebarKey, terminalKey, backgroundToneKey, surfaceToneKey] {
            defaults.removeObject(forKey: key)
        }
    }

    /// Write a workspace's transparency values into ITS suite; a running
    /// instance applies them on its `/reload-appearance` ping.
    static func write(
        background: Double,
        surface: Double,
        backgroundTone: Double,
        surfaceTone: Double,
        to defaults: UserDefaults
    ) {
        defaults.set(background, forKey: frameKey)
        defaults.set(surface, forKey: terminalKey)
        defaults.set(backgroundTone, forKey: backgroundToneKey)
        defaults.set(surfaceTone, forKey: surfaceToneKey)
    }

    /// Re-read OWN suite after a peer wrote it (the scoped Appearance editor
    /// in another workspace's window) and re-apply/announce exactly like a
    /// local slider change would.
    func reloadFromDefaults() {
        let values = Self.savedValues(in: AppDefaults.shared)
        applyScopedPresentation(
            background: values.background,
            surface: values.surface,
            backgroundTone: values.backgroundTone,
            surfaceTone: values.surfaceTone
        )
    }

    /// Apply the active Host's appearance without materializing it as this
    /// Controller workspace's own preference. Returning to Local calls
    /// `reloadFromDefaults`, restoring the untouched local values.
    func applyScopedPresentation(
        background: Double,
        surface: Double,
        backgroundTone: Double,
        surfaceTone: Double
    ) {
        suppressPersistence = true
        if abs(backgroundOpacity - background) > 0.0001 {
            backgroundOpacity = background
        }
        if abs(surfaceOpacity - surface) > 0.0001 {
            surfaceOpacity = surface
        }
        if abs(self.backgroundTone - backgroundTone) > 0.0001 {
            self.backgroundTone = backgroundTone
        }
        if abs(self.surfaceTone - surfaceTone) > 0.0001 {
            self.surfaceTone = surfaceTone
        }
        suppressPersistence = false
        NotificationCenter.default.post(name: .unpeelTransparencyChanged, object: nil)
    }

    /// Persist immediately; announce on a short trailing debounce so slider
    /// drags don't rebuild every cached Ghostty pane's config per tick.
    private func valueChanged() {
        guard !suppressPersistence else { return }
        let defaults = AppDefaults.shared
        defaults.set(backgroundOpacity, forKey: Self.frameKey)
        defaults.set(surfaceOpacity, forKey: Self.terminalKey)
        defaults.set(backgroundTone, forKey: Self.backgroundToneKey)
        defaults.set(surfaceTone, forKey: Self.surfaceToneKey)

        announceWorkItem?.cancel()
        let work = DispatchWorkItem {
            NotificationCenter.default.post(
                name: .unpeelTransparencyChanged, object: nil
            )
        }
        announceWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
    }
}

/// Invalidation bridge for chrome views with no store dependency
/// (SidebarBackground, ContentBackground, SettingsMainBackground): they are
/// stateless, so SwiftUI never re-runs their bodies on its own and an App
/// color change would leave them at the last resolved wash. Observing this
/// object makes the tint a real SwiftUI dependency.
@MainActor
final class AppTintModel: ObservableObject {
    static let shared = AppTintModel()

    @Published private(set) var generation = 0

    private var observer: NSObjectProtocol?

    private init() {
        observer = NotificationCenter.default.addObserver(
            forName: .unpeelAppTintChanged, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.generation += 1
            }
        }
    }

    /// Per-frame invalidation during a tint animation (the notification is
    /// reserved for the throttled, expensive consumers — Ghostty restyles).
    fileprivate func bump() {
        generation += 1
    }
}

/// Eases App color changes instead of snapping them: hue travels the
/// shortest arc, wash strength ramps for neutral ↔ color transitions. The
/// SwiftUI chrome re-resolves every tick (cheap — three background views),
/// while `.unpeelAppTintChanged` fires only at completion, because each post
/// makes SurfaceCache rebuild Ghostty configs for every cached pane on the
/// main thread. A workspace switch to a different-colored Host should land
/// on this same path without multiplying that expensive work across frames.
@MainActor
final class AppTintAnimator {
    static let shared = AppTintAnimator()

    private var timer: Timer?
    private var pendingCompletion: (() -> Void)?

    func cancel() {
        timer?.invalidate()
        timer = nil
    }

    func animate(
        toHue: Double?,
        duration: TimeInterval = 0.35,
        completion: (() -> Void)? = nil
    ) {
        cancel()
        pendingCompletion = completion
        let fromHue = Theme.appTintHue
        let fromStrength = fromHue == nil ? 0 : Theme.appTintStrength
        guard fromHue != nil || toHue != nil else {
            finish(toHue: toHue)
            return
        }
        let startedAt = Date()
        var tick = 0
        let timer = Timer(timeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                let raw = min(1, Date().timeIntervalSince(startedAt) / duration)
                // Ease in-out.
                let t = raw < 0.5
                    ? 2 * raw * raw
                    : 1 - pow(-2 * raw + 2, 2) / 2
                Theme.appTintStrength =
                    fromStrength + ((toHue == nil ? 0 : 1) - fromStrength) * t
                Theme.appTintHue = Self.interpolatedHue(
                    from: fromHue, to: toHue, progress: t
                )
                AppTintModel.shared.bump()
                // Every 3rd tick (~10Hz): let the on-screen canvases track
                // the fade (SurfaceCache restyles visible panes only — the
                // full pass still waits for completion). Without this the
                // terminals, the largest surface on screen, hold the old
                // color through the whole fade and cut over in one step.
                tick += 1
                if tick % 3 == 0 {
                    NotificationCenter.default.post(
                        name: .unpeelAppTintAnimationTick, object: nil
                    )
                }
                if raw >= 1 {
                    self.cancel()
                    self.finish(toHue: toHue)
                }
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func finish(toHue: Double?) {
        Theme.appTintHue = toHue
        Theme.appTintStrength = 1
        let completion = pendingCompletion
        pendingCompletion = nil
        NotificationCenter.default.post(name: .unpeelAppTintChanged, object: nil)
        completion?()
    }

    /// Finger-scrubbed wash for the interactive workspace swipe: sets the
    /// chrome blend between the drag-start values and the destination hue,
    /// with no timer. `progress` 1 = the swipe's commit distance, but the
    /// color deliberately TRAILS the fingers — a quadratic curve capped at
    /// ~60% blend at the commit point — so most of the hue change lands in
    /// the settle animation after the switch (applyScopeTint's 0.5s
    /// `animate`), not during the drag; a cancel eases the partial wash
    /// home. Cancels any running animation and invalidates only the cheap
    /// chrome — the expensive Ghostty restyle (`.unpeelAppTintChanged`)
    /// waits for the `animate` call that a commit (applyScopeTint) or
    /// cancel (restore) settles with.
    func scrubWash(
        fromHue: Double?,
        fromStrength: Double,
        toHue: Double?,
        progress: Double
    ) {
        cancel()
        let clamped = min(1, max(0, progress))
        // progress² × 0.6: barely tinted through the first half of the
        // drag, ~60% blended when releasing would commit.
        let trailing = 0.6 * clamped * clamped
        let baseStrength = fromHue == nil ? 0 : fromStrength
        Theme.appTintStrength =
            baseStrength + ((toHue == nil ? 0 : 1) - baseStrength) * trailing
        Theme.appTintHue = Self.interpolatedHue(
            from: fromHue, to: toHue, progress: trailing
        )
        AppTintModel.shared.bump()
    }

    /// From/to on the hue circle via the shortest arc. Neutral ↔ color rides
    /// the strength ramp at the colored end's hue.
    private static func interpolatedHue(
        from: Double?, to: Double?, progress: Double
    ) -> Double? {
        switch (from, to) {
        case (nil, nil):
            return nil
        case (nil, let target?):
            return target
        case (let origin?, nil):
            return origin
        case (let origin?, let target?):
            let delta = (target - origin + 540)
                .truncatingRemainder(dividingBy: 360) - 180
            let hue = origin + delta * progress
            return (hue + 360).truncatingRemainder(dividingBy: 360)
        }
    }
}

enum Theme {
    // MARK: Chrome / layout

    static let titlebarHeight: CGFloat = 38
    /// Compact window-chrome strip above the panes (project breadcrumb +
    /// Open-in chips). Library pages keep the classic 38pt titlebar.
    static let titleStripHeight: CGFloat = 30
    /// Shared height for sidebar Session buttons and split-pane headers.
    static let sessionRowHeight: CGFloat = 28
    static let sidebarDefaultWidth: CGFloat = 300
    static let sidebarMinWidth: CGFloat = 220
    static let sidebarMaxWidth: CGFloat = 520

    /// The stock macOS window corner radius (16pt on macOS 26). The
    /// terminal pane's leading corners are rounded with this so its left
    /// edge mirrors the window's own rounding on the right. AppDelegate
    /// overwrites the default with the radius read off the real window
    /// frame at launch, so it tracks whatever the running OS uses.
    @MainActor static var windowCornerRadius: CGFloat = 16

    /// Gap between the content pane (Surface) and the window's top/right/
    /// bottom edges, showing the Background backdrop around it.
    static let surfaceInset: CGFloat = 8

    /// Shared corner radius for the content-area clip AND the terminal pane
    /// cards inside it — one value so a solo pane's card corners land
    /// exactly on the content clip with no frame-material slivers.
    static let contentCornerRadius: CGFloat = 10

    /// Hairline rim shared by the pane cards and the full-content screens
    /// (settings, libraries, launcher): currentColor at low opacity so it
    /// adapts to light/dark.
    static var contentHairline: Color { .primary.opacity(0.15) }

    /// The content pane's clip shape: uniform `contentCornerRadius` on every
    /// corner, matching the pane-card chrome (the `inset` parameter is kept
    /// for call-site stability; the radius no longer derives from it).
    /// Shared by the pane's content clip and its backdrop, so the backdrop
    /// can never poke square corners into the inset gap.
    @MainActor static func contentPaneShape(inset _: CGFloat) -> UnevenRoundedRectangle {
        UnevenRoundedRectangle(
            topLeadingRadius: contentCornerRadius,
            bottomLeadingRadius: contentCornerRadius,
            bottomTrailingRadius: contentCornerRadius,
            topTrailingRadius: contentCornerRadius,
            style: .continuous
        )
    }

    /// Shared factory so SwiftUI tokens and layer-backed AppKit consumers
    /// resolve from the same dynamic provider.
    static func dynamicNSColor(light: NSColor, dark: NSColor) -> NSColor {
        NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .aqua ? light : dark
        }
    }

    // MARK: App tint engine

    /// The workspace tint hue in effect (degrees), mirrored from
    /// `UnpeelStore.appTint`. Written on the main thread (launch + Appearance
    /// panel); read inside dynamic-color resolve closures on whatever thread
    /// AppKit resolves on — a torn read is benign (one frame of the old wash).
    nonisolated(unsafe) static var appTintHue: Double?

    /// Mirror of `TransparencyModel.shared.surfaceOpacity` kept in a
    /// nonisolated static (same pattern as `appTintHue`) so
    /// `TerminalPaneStyle.resolved()` and layer-level helpers can read it
    /// without actor hops. Written only by TransparencyModel on the main
    /// thread.
    nonisolated(unsafe) static var terminalBackgroundOpacity: Double = 1

    /// Mirror of the Surface tone override (nil = design detent / "Auto").
    /// Non-nil replaces the default terminal canvas and page surfaces with
    /// `toneNSColor(_:)` at that brightness in both appearances. Written
    /// only by TransparencyModel on the main thread.
    nonisolated(unsafe) static var surfaceToneOverride: Double?

    /// The shared neutral used by the tone sliders: one hue/saturation for
    /// Background and Surface alike (saturation fades toward white), so
    /// equal tones are the identical color and the boundary between areas
    /// disappears. Absolute — deliberately NOT appearance-dynamic.
    nonisolated static func toneNSColor(_ tone: Double) -> NSColor {
        let t = min(max(tone, 0), 1)
        let base = NSColor(
            hue: 232.0 / 360.0,
            saturation: 0.16 * (1 - t),
            brightness: t,
            alpha: 1
        )
        // Pin to sRGB so the hex handed to Ghostty and the color painted by
        // the chrome resolve to the same pixels (the lockstep requirement).
        return base.usingColorSpace(.sRGB) ?? base
    }

    /// Appearance-dynamic tone paint for chrome views: custom tones shape
    /// the DARK appearance only. Light mode keeps its designed white
    /// surfaces — an absolute dark tone under light mode's dark text made
    /// the whole UI unreadable the moment the appearance flipped.
    static func toneColor(_ tone: Double) -> Color {
        Color(light: NSColor(hex: 0xFFFFFF), dark: toneNSColor(tone))
    }

    nonisolated static func toneHexString(_ tone: Double) -> String {
        let color = toneNSColor(tone).usingColorSpace(.sRGB) ?? toneNSColor(tone)
        return String(
            format: "#%02X%02X%02X",
            Int(round(color.redComponent * 255)),
            Int(round(color.greenComponent * 255)),
            Int(round(color.blueComponent * 255))
        )
    }

    /// Just the workspace App-color glass wash (no neutral base), for
    /// tone-overridden surfaces: they skip the per-area neutral tints (whose
    /// differences would break the flat look) but must still carry the
    /// workspace color, applied identically everywhere.
    static var workspaceGlassWash: Color {
        guard let hue = appTintHue else { return .clear }
        let wash = appTintGlassWash(hue: hue)
        return Color(light: wash.light, dark: wash.dark)
    }

    /// Wash intensity 0…1, animated by AppTintAnimator for neutral ↔ color
    /// transitions; 1 whenever a tint is at rest. Only meaningful while
    /// `appTintHue` is non-nil.
    nonisolated(unsafe) static var appTintStrength: Double = 1

    /// Neutral-chrome wash: keep the base color's brightness and alpha, push
    /// its hue to the tint with a brightness-scaled saturation. The curve must
    /// be steep in the darks: perceived chroma ≈ saturation × brightness, so a
    /// near-black like #1A1A1F (b ≈ 0.12) needs s ≈ 0.5 for a *visible* cast
    /// while near-white text/paper wants only ~0.1. Mirror any change into the
    /// iOS twin (TerminalChrome.hostTinted) or phone and Mac chrome drift.
    static func appTinted(_ base: NSColor) -> NSColor {
        appTinted(base, washScale: 1)
    }

    /// The terminal canvas takes a much lighter wash than the chrome
    /// (2026-09-01): the reading surface should match the website's neutral
    /// dark cards — gray with only a hint of the workspace color — while
    /// the frame around it carries the color. Scale chosen so the dark
    /// canvas lands near s ≈ 0.17 instead of the chrome's 0.5 cap.
    static let canvasWashScale = 0.35

    static func appTintedCanvas(_ base: NSColor) -> NSColor {
        appTinted(base, washScale: canvasWashScale)
    }

    private static func appTinted(_ base: NSColor, washScale: Double) -> NSColor {
        guard let hueDegrees = appTintHue,
              let srgb = base.usingColorSpace(.sRGB) else { return base }
        var h: CGFloat = 0, s: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        srgb.getHue(&h, saturation: &s, brightness: &b, alpha: &a)
        // Steeper still at the bright end (2026-08-23): near-white paper
        // takes only ~2.5% cast so light-mode surfaces read as white for
        // EVERY App color (6% still read visibly colored on warm hues);
        // darks keep their ~50% cap and the midrange is unchanged
        // (b=0.5 → 0.32, same as before).
        let wash = max(s, min(0.5, 0.615 - 0.59 * b) * appTintStrength * washScale)
        return srgbColor(
            hue: hueDegrees / 360, saturation: wash, brightness: b, alpha: a
        )
    }

    /// `appTinted` over a "#RRGGBB" string (the Ghostty theme path — the
    /// surface paints these strings directly, so they must track the tinted
    /// chrome background exactly or the pane reads as an inset patch).
    static func appTintedHexString(_ hex: String) -> String {
        appTintedHexString(hex, washScale: 1)
    }

    /// Canvas-wash twin for the Ghostty background strings — must stay in
    /// lockstep with `terminalBackgroundNSColor`'s canvas wash.
    static func appTintedCanvasHexString(_ hex: String) -> String {
        appTintedHexString(hex, washScale: canvasWashScale)
    }

    private static func appTintedHexString(_ hex: String, washScale: Double) -> String {
        var body = hex
        if body.hasPrefix("#") { body.removeFirst() }
        guard appTintHue != nil, body.count == 6,
              let rgb = UInt32(body, radix: 16),
              let tinted = appTinted(NSColor(hex: rgb), washScale: washScale)
                  .usingColorSpace(.sRGB)
        else { return hex }
        return String(
            format: "#%02x%02x%02x",
            Int(round(tinted.redComponent * 255)),
            Int(round(tinted.greenComponent * 255)),
            Int(round(tinted.blueComponent * 255))
        )
    }

    /// HSB → sRGB by hand so tinted grays stay in the same color space as the
    /// hex tokens (NSColor's hue initializer would land in generic RGB).
    private static func srgbColor(
        hue: CGFloat, saturation: CGFloat, brightness: CGFloat, alpha: CGFloat
    ) -> NSColor {
        let h = (hue - floor(hue)) * 6
        let f = h - floor(h)
        let p = brightness * (1 - saturation)
        let q = brightness * (1 - saturation * f)
        let t = brightness * (1 - saturation * (1 - f))
        let (r, g, b): (CGFloat, CGFloat, CGFloat)
        switch Int(h) % 6 {
        case 0: (r, g, b) = (brightness, t, p)
        case 1: (r, g, b) = (q, brightness, p)
        case 2: (r, g, b) = (p, brightness, t)
        case 3: (r, g, b) = (p, q, brightness)
        case 4: (r, g, b) = (t, p, brightness)
        default: (r, g, b) = (brightness, p, q)
        }
        return NSColor(srgbRed: r, green: g, blue: b, alpha: alpha)
    }

    /// Dynamic chrome color that applies the workspace tint at resolve time.
    /// Tokens built with this are computed vars, so a tint change hands
    /// SwiftUI fresh values (forcing a repaint) AND late resolvers pick the
    /// current hue.
    static func tintedDynamicNSColor(light: NSColor, dark: NSColor) -> NSColor {
        NSColor(name: nil) { appearance in
            appTinted(
                appearance.bestMatch(from: [.aqua, .darkAqua]) == .aqua
                    ? light : dark
            )
        }
    }

    private static func tintedColor(light: NSColor, dark: NSColor) -> Color {
        Color(nsColor: tintedDynamicNSColor(light: light, dark: dark))
    }

    // MARK: Colors (DESIGN.md §2; light values from glass.css [data-theme="light"])

    // Neutral chrome tokens below are computed vars through the app-tint
    // wash (semantic colors — attention/unread/danger/accent — stay fixed).

    /// Primary text: light #111217, dark #F3F5FB
    static var foreground: Color {
        tintedColor(light: NSColor(hex: 0x111217), dark: NSColor(hex: 0xF3F5FB))
    }
    /// Muted/secondary text: foreground @ 60% light / 66% dark
    static var mutedForeground: Color {
        tintedColor(
            light: NSColor(hex: 0x111217, opacity: 0.60),
            dark: NSColor(hex: 0xF3F5FB, opacity: 0.66)
        )
    }
    /// The two dark planes. Background frames the window; Surface carries
    /// terminal canvases and pages above it. Linear-style hierarchy
    /// (2026-09-01): the frame sits BELOW the surface — a near-black sidebar
    /// with slightly lighter content cards — instead of the old lighter
    /// #2B2E37 frame around darker #1A1A1F terminals.
    static let darkBackgroundHex: UInt32 = 0x121314
    static let darkSurfaceHex: UInt32 = 0x1A1B1D
    static let darkSurfaceHexString = "#1A1B1D"
    /// Dark surface base while an App color is active: the tint wash keeps
    /// the base's brightness, and a saturated near-black reads heavier than
    /// the neutral gray does, so colored canvases start a step lighter to
    /// hold the same canvas-above-frame hierarchy.
    static let darkTintedSurfaceHex: UInt32 = 0x1F2023

    /// The dark surface base for the CURRENT tint state, brightness-lerped
    /// by the tint strength ramp so enabling an App color never pops the
    /// canvas before the wash fades in. Both canvas paints — the chrome twin
    /// (`terminalBackgroundNSColor`) and the Ghostty theme string
    /// (`TerminalPaneStyle.resolved`) — read this one value so they can
    /// never drift apart.
    static var currentDarkSurfaceHex: UInt32 {
        guard appTintHue != nil else { return darkSurfaceHex }
        let t = min(max(appTintStrength, 0), 1)
        func channel(_ shift: UInt32) -> UInt32 {
            let a = Double((darkSurfaceHex >> shift) & 0xFF)
            let b = Double((darkTintedSurfaceHex >> shift) & 0xFF)
            return UInt32((a + (b - a) * t).rounded()) << shift
        }
        return channel(16) | channel(8) | channel(0)
    }
    static var currentDarkSurfaceHexString: String {
        String(format: "#%06x", currentDarkSurfaceHex)
    }

    /// Terminal surface (opaque): light #ffffff, dark #1A1B1D. NSColor twin
    /// for layer-backed views (TerminalHostView.SwapContainer). Since the
    /// 2026-09-01 Linear-style hierarchy the cards sit slightly ABOVE the
    /// near-black frame; the canvas board inherits it as its surface.
    static var terminalBackgroundNSColor: NSColor {
        // Surface tone override (dark appearance only — see toneColor): the
        // chrome twin must stay in lockstep with the Ghostty canvas
        // (TerminalPaneStyle.resolved reads the same override), or the
        // titlebar/swap fills show as mismatched patches.
        if let tone = surfaceToneOverride {
            return canvasTintedDynamicNSColor(
                light: NSColor(hex: 0xFFFFFF), dark: toneNSColor(tone)
            )
        }
        return canvasTintedDynamicNSColor(
            light: NSColor(hex: 0xFFFFFF), dark: NSColor(hex: currentDarkSurfaceHex)
        )
    }

    /// `tintedDynamicNSColor` with the canvas wash: the terminal surface and
    /// every chrome twin that must match it resolve through this, so the
    /// gray canvas can never drift from its own titlebar/swap fills.
    private static func canvasTintedDynamicNSColor(
        light: NSColor, dark: NSColor
    ) -> NSColor {
        NSColor(name: nil) { appearance in
            appTintedCanvas(
                appearance.bestMatch(from: [.aqua, .darkAqua]) == .aqua
                    ? light : dark
            )
        }
    }
    static var terminalBackground: Color {
        Color(nsColor: terminalBackgroundNSColor)
    }
    /// Opaque app fallback: light #ffffff, dark #121314
    static var appBackground: Color {
        tintedColor(
            light: NSColor(hex: 0xFFFFFF), dark: NSColor(hex: darkBackgroundHex)
        )
    }
    /// Hover row bg: foreground @ 10% in both modes
    static var hoverRow: Color {
        tintedColor(
            light: NSColor(hex: 0x111217, opacity: 0.10),
            dark: NSColor(hex: 0xF3F5FB, opacity: 0.10)
        )
    }
    /// Active/selected row bg: light solid white (--glass-active-tint),
    /// dark rgba(255,255,255,0.16)
    static var activeRow: Color {
        tintedColor(
            light: NSColor(hex: 0xFFFFFF),
            dark: NSColor(hex: 0xFFFFFF, opacity: 0.16)
        )
    }
    /// Selected-row glass tint: neutral (activeRow) with no workspace color;
    /// with one, glass OF that color — the appTinted wash on activeRow is
    /// near-invisible at white's brightness (~2.5% saturation), so the
    /// colored chip needs its own saturated pair. Alpha stays moderate so
    /// row text keeps contrast on both the Liquid Glass chip and the flat
    /// pre-26 fallback fill.
    static var activeRowGlassTint: Color {
        guard let hue = appTintHue else { return activeRow }
        let strength = appTintStrength
        return Color(
            light: srgbColor(
                hue: hue / 360, saturation: 0.22 * strength,
                brightness: 0.98, alpha: 1
            ),
            dark: srgbColor(
                hue: hue / 360, saturation: 0.55 * strength,
                brightness: 0.52, alpha: 0.30
            )
        )
    }
    /// Attention dot (session) #f59e0b
    static let attention = Color(hex: 0xF59E0B)
    /// Unread badge #60a5fa
    static let unread = Color(hex: 0x60A5FA)
    /// Danger #ef4444
    static let danger = Color(hex: 0xEF4444)
    /// Control accent for native form controls (switch ON state) and the
    /// Quick badge — the Svelte quick-launch green (PresetsPanel.svelte
    /// badge #34C759, same hue as the system switch green).
    static let accent = Color(hex: 0x34C759)
    /// Neutral CTA tint for prominent glass buttons and segmented selection
    /// (designer's spec 2026-06-12: CTAs in the app gray, not system blue).
    /// Light gray reads as "primary" against the dark cards — the old
    /// hand-rolled Save was white-20%; glassProminent needs a near-white
    /// fill to get the same emphasis with an auto-dark label. (A/B'd
    /// 2026-06-12 against the darker app gray #555C6F: that one rendered
    /// nearly identical to the .bordered secondary next to it, killing the
    /// primary/secondary hierarchy.) Light mode inverts to near-black
    /// (--primary light #111217) for the same emphasis with an auto-light
    /// label.
    static var ctaTint: Color {
        tintedColor(
            light: NSColor(hex: 0x111217), dark: NSColor(white: 0.85, alpha: 1)
        )
    }
    /// Sidebar resizer hairline: dark ≈ rgba(255,255,255,0.055), light a
    /// subtle dark hairline
    static var resizerLine: Color {
        tintedColor(
            light: NSColor(hex: 0x000000, opacity: 0.08),
            dark: NSColor(hex: 0xFFFFFF, opacity: 0.055)
        )
    }
    /// Terminal pane separators: the hairline under the shared titlebar /
    /// pane headers and the split divider line. Clearly white in dark mode
    /// (the near-invisible resizerLine read as a dark seam against the
    /// canvas), one token so they always match.
    static var paneDividerLine: Color {
        tintedColor(
            light: NSColor(hex: 0x000000, opacity: 0.10),
            dark: NSColor(hex: 0xFFFFFF, opacity: 0.14)
        )
    }
    /// Hovered split divider: stronger in the same direction (whiter in
    /// dark, darker in light) so the grab strip announces itself.
    static var paneDividerLineHover: Color {
        tintedColor(
            light: NSColor(hex: 0x000000, opacity: 0.30),
            dark: NSColor(hex: 0xFFFFFF, opacity: 0.36)
        )
    }
    /// Generic busy spinner fg/muted mix: dark ≈ #B9BDC9, light ≈ #4A4D55
    static let genericSpinner = Color(
        light: NSColor(hex: 0x4A4D55), dark: NSColor(hex: 0xB9BDC9)
    )
    /// Settings shell dim over the content tint (.settings-main-shell):
    /// dark black @ 24%, light white @ 36% (SettingsView.svelte).
    static let settingsShellDim = Color(
        light: NSColor(hex: 0xFFFFFF, opacity: 0.36),
        dark: NSColor(hex: 0x000000, opacity: 0.24)
    )

    // MARK: Tint overlays painted over vibrancy (DESIGN.md §1)

    // Dark: tune the raw sidebar material toward the content pane so the
    // rounded sidebar boundary does not read as a dark full-height slab. The
    // sidebar list fade is handled by `SidebarListFadeMask`, not by these
    // tint overlays.
    //
    // Each wash below is a historical STACK of uniform tints flattened into
    // the single equivalent color: alpha-compositing two flat colors yields
    // one flat color, and each dropped layer was a full-surface blend pass
    // per frame. The component tints are kept inline so the DESIGN.md tokens
    // stay legible.

    /// Alpha-composite `top` over `bottom` (straight alpha, sRGB — the space
    /// CA blends layers in), so the merged wash renders identically to the
    /// old two-layer stack.
    private static func flattened(_ bottom: NSColor, _ top: NSColor) -> NSColor {
        guard let b = bottom.usingColorSpace(.sRGB),
              let t = top.usingColorSpace(.sRGB) else { return top }
        let ab = b.alphaComponent, at = t.alphaComponent
        let outA = at + ab * (1 - at)
        guard outA > 0 else { return .clear }
        func mix(_ tc: CGFloat, _ bc: CGFloat) -> CGFloat {
            (tc * at + bc * ab * (1 - at)) / outA
        }
        return NSColor(
            srgbRed: mix(t.redComponent, b.redComponent),
            green: mix(t.greenComponent, b.greenComponent),
            blue: mix(t.blueComponent, b.blueComponent),
            alpha: outA
        )
    }

    /// Sidebar: subdued neutral wash; light mode keeps a soft white glass
    /// tint. Bottom #FFFFFF@26% / #0C0D0E@50%, top #FFFFFF@18% / #111214@55%.
    /// The dark wash is deliberately heavy (~78% combined): the raw sidebar
    /// material reads mid-gray, and the Linear-style frame needs it pinned
    /// near #121314 with only a faint desktop-vibrancy bleed.
    private static let sidebarTintBase = (
        light: flattened(
            NSColor(hex: 0xFFFFFF, opacity: 0.26),
            NSColor(hex: 0xFFFFFF, opacity: 0.18)
        ),
        dark: flattened(
            NSColor(hex: 0x0C0D0E, opacity: 0.50),
            NSColor(hex: 0x111214, opacity: 0.55)
        )
    )
    /// The saturated glass wash an active App color paints over the window
    /// chrome. Applied identically to the sidebar, content, and settings
    /// backdrops: SidebarBackground spans the whole window and the content
    /// vibrancy covers it from the corner-radius strip rightward, so any
    /// per-surface difference in this wash shows as a hard vertical seam at
    /// that strip. Same wash everywhere = only the materials differ, exactly
    /// like the neutral design. Alpha rides the animated strength ramp so
    /// neutral ↔ color fades in place.
    private static func appTintGlassWash(hue: Double) -> (light: NSColor, dark: NSColor) {
        let alpha = 0.55 * appTintStrength
        return (
            light: srgbColor(
                hue: hue / 360, saturation: 0.16, brightness: 0.97, alpha: alpha
            ),
            // Dark brightness must stay BELOW the tinted terminal canvas
            // (appTinted keeps darkSurfaceHex's ~11%): every App color keeps
            // the Linear-style hierarchy — near-black colored frame, slightly
            // lighter colored surface. 0.24 (pre-2026-09-01) read as a frame
            // LIGHTER than the canvas.
            dark: srgbColor(
                hue: hue / 360, saturation: 0.60, brightness: 0.11, alpha: alpha
            )
        )
    }

    /// Neutral base pair with the colored glass wash flattened on top.
    private static func tintWashed(
        _ base: (light: NSColor, dark: NSColor), hue: Double
    ) -> Color {
        let wash = appTintGlassWash(hue: hue)
        return Color(
            light: flattened(base.light, wash.light),
            dark: flattened(base.dark, wash.dark)
        )
    }

    static var sidebarTint: Color {
        // With an App color active the window chrome CARRIES the color: not
        // the subtle run-everything-through-the-wash pass the other tokens
        // get, but a saturated glass wash over the vibrancy. Default keeps
        // the shipped neutral wash untouched.
        guard let hue = appTintHue else {
            return Color(light: sidebarTintBase.light, dark: sidebarTintBase.dark)
        }
        return tintWashed(sidebarTintBase, hue: hue)
    }
    /// Main content: dark #1A1B1D @ 12% over the frame hex @ 20%; light
    /// white 12%/32%
    private static let contentTintBase = (
        light: flattened(
            NSColor(hex: 0xFFFFFF, opacity: 0.32),
            NSColor(hex: 0xFFFFFF, opacity: 0.12)
        ),
        dark: flattened(
            NSColor(hex: darkBackgroundHex, opacity: 0.20),
            NSColor(hex: 0x1A1B1D, opacity: 0.12)
        )
    )
    static var contentTint: Color {
        guard let hue = appTintHue else {
            return Color(light: contentTintBase.light, dark: contentTintBase.dark)
        }
        return tintWashed(contentTintBase, hue: hue)
    }
    /// Settings shell (.settings-main-shell): the content tint pair over
    /// `settingsShellDim` (white@36% / black@24%), flattened the same way.
    private static let settingsShellTintBase = (
        light: flattened(
            flattened(
                NSColor(hex: 0xFFFFFF, opacity: 0.36),
                NSColor(hex: 0xFFFFFF, opacity: 0.32)
            ),
            NSColor(hex: 0xFFFFFF, opacity: 0.12)
        ),
        dark: flattened(
            flattened(
                NSColor(hex: 0x000000, opacity: 0.24),
                NSColor(hex: darkBackgroundHex, opacity: 0.20)
            ),
            NSColor(hex: 0x1A1B1D, opacity: 0.12)
        )
    )
    static var settingsShellTint: Color {
        guard let hue = appTintHue else {
            return Color(
                light: settingsShellTintBase.light,
                dark: settingsShellTintBase.dark
            )
        }
        return tintWashed(settingsShellTintBase, hue: hue)
    }

    // MARK: Tool brand colors (DESIGN.md §1/§5)

    /// The CLI's brand tint as a raw 0xRRGGBB, or nil when there is no
    /// per-tool brand color (plain terminals, unknown commands). This is the
    /// single color table: the local Color accessors below AND the phone wire
    /// (`RemoteSessionSummary.spinnerColorHex`) both read it, so a new CLI's
    /// color reaches every surface — including paired phones, with no phone
    /// update — by adding one line here.
    static func toolColorHex(forCommand command: String) -> Int? {
        UnpeelRuntimeCatalog.runtime(command: command)?.tintColorHex
            ?? installedAppTint(forCommand: command)?.tint
    }

    /// Provider-specific spinner treatments can differ from their brand marks.
    static func toolSpinnerColorHex(forCommand command: String) -> Int? {
        if let runtime = UnpeelRuntimeCatalog.runtime(command: command) {
            return runtime.spinnerTintColorHex ?? runtime.tintColorHex
        }
        return installedAppTint(forCommand: command)?.spinner
    }

    // MARK: Installed Unpeel App tints (Host-resolved catalog data)

    /// Command-keyed tints for installed Unpeel Apps, sourced ONLY from
    /// Host-stamped session manifests (`active_app`): the Host matched the
    /// central App catalog against its PATH, so native never guesses identity.
    /// Keyed by the launch command's executable basename; built-in catalog
    /// entries always win above. Guarded because the phone-wire DTO adapters
    /// read the color table off the main actor.
    private static let installedAppTintLock = NSLock()
    nonisolated(unsafe) private static var installedAppTints:
        [String: (tint: Int?, spinner: Int?)] = [:]

    static func updateInstalledAppTints(_ tints: [String: (tint: Int?, spinner: Int?)]) {
        installedAppTintLock.lock()
        installedAppTints = tints
        installedAppTintLock.unlock()
    }

    private static func installedAppTint(
        forCommand command: String
    ) -> (tint: Int?, spinner: Int?)? {
        let key = Self.commandBasename(command)
        guard !key.isEmpty else { return nil }
        installedAppTintLock.lock()
        defer { installedAppTintLock.unlock() }
        return installedAppTints[key]
    }

    /// First whitespace token's path basename, lowercased — the same
    /// normalization the Host's App detection index uses.
    static func commandBasename(_ command: String) -> String {
        guard let head = command.split(whereSeparator: { $0 == " " || $0 == "\t" }).first
        else { return "" }
        return head.split(separator: "/").last.map { $0.lowercased() } ?? ""
    }

    static func toolColor(forCommand command: String) -> Color {
        if let hex = toolColorHex(forCommand: command) { return Color(hex: UInt32(hex)) }
        if command.trimmingCharacters(in: .whitespaces).isEmpty {
            // Plain terminal: near-fg gray per mode.
            return Color(light: NSColor(hex: 0x4A4F5A), dark: NSColor(hex: 0xD6D9E1))
        }
        return genericSpinner
    }

    static func toolSpinnerColor(forCommand command: String) -> Color {
        if let hex = toolSpinnerColorHex(forCommand: command) { return Color(hex: UInt32(hex)) }
        return toolColor(forCommand: command)
    }

    // MARK: Sidebar row typography

    /// Session/folder row labels. The Svelte app's 12px/600 renders heavier
    /// in native SF Pro than in the web sidebar, so the native rows use
    /// 13pt medium instead (designer's call, 2026-06-12).
    static let rowLabelFont = Font.system(size: 13, weight: .medium)
    /// Session titles render at full foreground opacity (folders sit at
    /// 0.6), so medium reads optically bolder there — sessions drop one
    /// more weight to match (designer's call, 2026-06-12).
    static let sessionLabelFont = Font.system(size: 13, weight: .regular)
    /// NSFont twin for CALayer-based renderers (shimmer overlay) that must
    /// match rowLabelFont metrics exactly.
    @MainActor static let rowLabelNSFont = NSFont.systemFont(ofSize: 13, weight: .medium)

    // MARK: Spinner (DESIGN.md §5)

    static let spinnerFrames: [String] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
    static let spinnerInterval: TimeInterval = 0.12
}

/// Plain description of the terminal surface theme, one variant per
/// appearance. Consumed by GhosttyBridge (which translates it into a
/// Ghostty `TerminalTheme`, so the surface flips with the window
/// appearance); defined here so the values stay next to the rest of the
/// design tokens.
/// Values: DESIGN.md §3 (terminal/theme.ts light + dark, default scheme).
struct TerminalPaneStyle {
    struct Variant {
        var background: String
        var foreground: String
        var selectionBackground: String
        var cursorColor: String
        /// ANSI 0–15 (default-scheme surface overrides applied).
        var palette: [String]
    }

    /// Dark (default-scheme overrides: brightBlack #6e6e76).
    /// Background follows Theme.terminalBackgroundNSColor (#1A1B1D): the
    /// Ghostty surface paints this string directly, so it must match the
    /// chrome's surface color or the pane shows as a lighter inset patch.
    var dark = Variant(
        background: Theme.darkSurfaceHexString,
        foreground: "#fafafa",
        selectionBackground: "#3a3a40",
        cursorColor: "#fafafa",
        palette: [
            "#1c1c22", "#ef4444", "#22c55e", "#eab308",
            "#3b82f6", "#a855f7", "#06b6d4", "#a1a1aa",
            "#6e6e76", "#f87171", "#4ade80", "#facc15",
            "#60a5fa", "#c084fc", "#22d3ee", "#fafafa",
        ]
    )

    /// Light (terminal/theme.ts light, default scheme).
    var light = Variant(
        background: "#ffffff",
        foreground: "#09090b",
        selectionBackground: "#d4d4d8",
        cursorColor: "#09090b",
        palette: [
            "#09090b", "#dc2626", "#16a34a", "#ca8a04",
            "#2563eb", "#9333ea", "#0891b2", "#e4e4e7",
            "#71717a", "#ef4444", "#22c55e", "#eab308",
            "#3b82f6", "#a855f7", "#06b6d4", "#fafafa",
        ]
    )

    var fontSize: Float = 13
    /// Runtime descriptors opt into horizontal padding. The neutral terminal
    /// style is edge-to-edge so full-bleed TUIs can own their whole canvas.
    var windowPaddingX: Int = 0
    var windowPaddingY: Int = 0
    /// Ghostty `window-padding-balance`. Keep this FALSE (ghostty's own
    /// default): balanced padding re-splits the leftover pixels (view size
    /// mod cell size) around the grid on every resize, so during a window
    /// drag the whole text block shifts by a few pixels per frame — visible
    /// as the terminal "shaking". Unbalanced, the grid is pinned at the
    /// fixed top-left padding and the remainder accrues bottom/right
    /// (invisible: window-padding-color=extend paints it as canvas).
    var windowPaddingBalanced = false
    /// Ghostty `mouse-scroll-multiplier`, discrete (wheel-tick) field only —
    /// 3 is ghostty tip's own discrete default. Trackpad (precision) scroll
    /// is pinned to 1 in GhosttyBridge to match the Ghostty app's feel.
    var mouseScrollMultiplier: Int = 3
    /// nil = leave Ghostty's bundled default (JetBrains Mono).
    var fontFamily: String?
    /// Ghostty `background-opacity` (Settings ▸ Appearance ▸ Transparency).
    /// Below 1 the surface paints its canvas translucent AND every solid
    /// chrome fill that normally backstops it (pane frame layer, swap
    /// container, column background) goes clear — the surface carries the
    /// only canvas paint, so the effective alpha is exactly this value.
    var backgroundOpacity: Double = 1

    /// DESIGN.md font stack: JetBrains Mono bundled-first, SF Mono fallback.
    /// Ghostty itself bundles JetBrains Mono as its default face, so when
    /// neither is installed system-wide we leave fontFamily nil and still
    /// get JetBrains Mono.
    static func resolved(
        runtimeID: String? = nil,
        command: String? = nil
    ) -> TerminalPaneStyle {
        var style = TerminalPaneStyle()
        let runtime = UnpeelRuntimeCatalog.runtime(id: runtimeID)
            ?? command.flatMap { UnpeelRuntimeCatalog.runtime(command: $0) }
        style.windowPaddingX = runtime?.windowPaddingX ?? 0
        // Floor the engine value just above zero: visually identical to 0
        // (the chrome layers behind go fully clear at 0 anyway) without
        // betting on how every libghostty path treats an exact-0 alpha.
        style.backgroundOpacity = max(0.001, Theme.terminalBackgroundOpacity)
        // Surface tone (Appearance ▸ Transparency): canvas brightness for
        // the DARK variant only — light mode keeps its designed white so
        // dark-on-dark never happens on an appearance flip. Applied before
        // the workspace tint below so tone and App color compose like the
        // defaults do.
        if let tone = Theme.surfaceToneOverride {
            style.dark.background = Theme.toneHexString(tone)
        } else {
            // Tinted canvases start from the lighter tinted-surface base —
            // the same value the chrome twin (terminalBackgroundNSColor)
            // resolves, so the pane never reads as an inset patch.
            style.dark.background = Theme.currentDarkSurfaceHexString
        }
        // Workspace tint: only the canvas colors wash — text, cursor, and the
        // ANSI palette keep their exact values so terminal content is never
        // recolored. Backgrounds use the CANVAS wash (gray with a hint of
        // color, matching the website's neutral dark cards) and must stay in
        // lockstep with Theme.terminalBackgroundNSColor (same transform on
        // the same base hex); selection keeps the full chrome wash.
        style.dark.background = Theme.appTintedCanvasHexString(style.dark.background)
        style.dark.selectionBackground =
            Theme.appTintedHexString(style.dark.selectionBackground)
        style.light.background = Theme.appTintedCanvasHexString(style.light.background)
        style.light.selectionBackground =
            Theme.appTintedHexString(style.light.selectionBackground)
        for (psName, family) in [("JetBrainsMono-Regular", "JetBrains Mono"),
                                 ("SFMono-Regular", "SF Mono")] {
            if NSFont(name: psName, size: 13) != nil {
                style.fontFamily = family
                break
            }
        }
        return style
    }
}
