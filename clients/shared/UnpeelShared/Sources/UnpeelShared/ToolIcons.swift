import Foundation

/// A resolved, client-renderable runtime icon. Provider artwork is generated
/// from `runtimes/<slug>/assets/icon.svg`; this type owns only generic agent
/// and terminal fallbacks so adding a runtime never requires a Swift case.
public struct UnpeelToolIcon: Equatable, Hashable, Identifiable, Sendable, CaseIterable {
    public let id: String
    public let key: String
    public let label: String
    public let kind: UnpeelRuntimeKind
    public let svgSource: String
    public let isTemplate: Bool
    public let fallbackSystemName: String
    public let usesRuntimeAsset: Bool

    public static var allCases: [UnpeelToolIcon] {
        UnpeelRuntimeCatalog.runtimes.map(forRuntime) + [.terminal]
    }

    /// Runtime art first, then an installed Unpeel App (by Host-stamped App
    /// id, else by the command's leading binary), else the terminal mark.
    public static func resolving(
        appID: String? = nil,
        providerID: String?,
        command: String
    ) -> UnpeelToolIcon {
        let runtime = UnpeelRuntimeCatalog.runtime(id: providerID)
            ?? UnpeelRuntimeCatalog.runtime(command: command)
        if let runtime {
            return forRuntime(runtime)
        }
        return UnpeelAppIconCatalog.icon(appID: appID)
            ?? UnpeelAppIconCatalog.icon(command: command)
            ?? .terminal
    }

    /// An installed Unpeel App's mark from the Host's catalog: the
    /// registry-authored SVG when it ships one, else the generic App mark.
    public static func forApp(id: String, name: String, iconSVG: String?) -> UnpeelToolIcon {
        let authored = iconSVG?.trimmingCharacters(in: .whitespacesAndNewlines)
        let hasAuthored = authored?.isEmpty == false
        return UnpeelToolIcon(
            id: "app:\(id)",
            key: id,
            label: name,
            kind: .app,
            svgSource: hasAuthored ? authored! : genericSVG(for: .app),
            isTemplate: true,
            fallbackSystemName: fallbackSystemName(for: .app),
            usesRuntimeAsset: hasAuthored
        )
    }

    public static func forRuntime(_ runtime: UnpeelRuntimeMetadata) -> UnpeelToolIcon {
        let authoredSVG = runtime.iconSVG?.trimmingCharacters(in: .whitespacesAndNewlines)
        let hasAuthoredSVG = authoredSVG?.isEmpty == false
        return UnpeelToolIcon(
            id: runtime.stableID,
            key: runtime.iconKey,
            label: runtime.label,
            kind: runtime.kind,
            svgSource: hasAuthoredSVG ? authoredSVG! : genericSVG(for: runtime.kind),
            // The generic fallback is always monochrome regardless of a
            // malformed descriptor's rendering hint.
            isTemplate: hasAuthoredSVG ? runtime.iconIsTemplate : true,
            fallbackSystemName: fallbackSystemName(for: runtime.kind),
            usesRuntimeAsset: hasAuthoredSVG
        )
    }

    public static let terminal = UnpeelToolIcon(
        id: "terminal",
        key: "terminal",
        label: "Terminal",
        kind: .terminal,
        svgSource: genericSVG(for: .terminal),
        isTemplate: true,
        fallbackSystemName: fallbackSystemName(for: .terminal),
        usesRuntimeAsset: false
    )

    /// Kind-owned generic marks so a future markdown-editor or Unpeel App
    /// CLI does not inherit the agent sparkle when it ships without art.
    private static func genericSVG(for kind: UnpeelRuntimeKind) -> String {
        switch kind {
        case .agent:
            return ##"<svg width="16" height="16" viewBox="0 0 24 24" fill="#FFFFFF" xmlns="http://www.w3.org/2000/svg"><path d="M12 2l1.64 5.36L19 9l-5.36 1.64L12 16l-1.64-5.36L5 9l5.36-1.64L12 2Z"/><path d="M19 15l.82 2.18L22 18l-2.18.82L19 21l-.82-2.18L16 18l2.18-.82L19 15Z"/></svg>"##
        case .app:
            return ##"<svg width="16" height="16" viewBox="0 0 256 256" fill="#FFFFFF" xmlns="http://www.w3.org/2000/svg"><path d="M208,40H48A16,16,0,0,0,32,56V200a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V56A16,16,0,0,0,208,40Zm0,16V88H48V56ZM48,200V104H208v96Z"/></svg>"##
        case .editor:
            return ##"<svg width="16" height="16" viewBox="0 0 256 256" fill="#FFFFFF" xmlns="http://www.w3.org/2000/svg"><path d="M208,24H72A16,16,0,0,0,56,40V216a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V40A16,16,0,0,0,208,24Zm0,192H72V40H208ZM96,80h80a8,8,0,0,1,0,16H96a8,8,0,0,1,0-16Zm0,40h80a8,8,0,0,1,0,16H96a8,8,0,0,1,0-16Zm0,40h48a8,8,0,0,1,0,16H96a8,8,0,0,1,0-16Z"/></svg>"##
        case .terminal:
            return ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M116,132.48l-72,64a6,6,0,0,1-8-9L103,128,36,68.49a6,6,0,0,1,8-9l72,64a6,6,0,0,1,0,9ZM216,186H120a6,6,0,0,0,0,12h96a6,6,0,0,0,0-12Z"></path></svg>"##
        }
    }

    private static func fallbackSystemName(for kind: UnpeelRuntimeKind) -> String {
        switch kind {
        case .agent: return "sparkles"
        case .app: return "square.stack"
        case .editor: return "doc.plaintext"
        case .terminal: return "terminal"
        }
    }
}

/// The Host's App catalog as icons. Fed from every bootstrap snapshot
/// (`availableApps`) by each client's store; read from any thread by the
/// icon resolvers, which is why it is lock-protected rather than actor-bound.
public enum UnpeelAppIconCatalog {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var byAppID: [String: UnpeelToolIcon] = [:]
    nonisolated(unsafe) private static var byBinary: [String: UnpeelToolIcon] = [:]

    public static func update(_ apps: [RemoteAppSummary]) {
        var nextByID: [String: UnpeelToolIcon] = [:]
        var nextByBinary: [String: UnpeelToolIcon] = [:]
        for app in apps {
            let icon = UnpeelToolIcon.forApp(id: app.id, name: app.name, iconSVG: app.iconSvg)
            nextByID[app.id.lowercased()] = icon
            let binary = (app.command as NSString).lastPathComponent.lowercased()
            if !binary.isEmpty {
                nextByBinary[binary] = icon
            }
        }
        lock.lock()
        byAppID = nextByID
        byBinary = nextByBinary
        lock.unlock()
    }

    public static func icon(appID: String?) -> UnpeelToolIcon? {
        guard let appID, !appID.isEmpty else { return nil }
        lock.lock()
        defer { lock.unlock() }
        return byAppID[appID.lowercased()]
    }

    /// Match the command's leading word by binary name, so both the bare
    /// launch-list command and a Host launch by absolute path resolve.
    public static func icon(command: String) -> UnpeelToolIcon? {
        guard let token = command
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(whereSeparator: { $0.isWhitespace })
            .first
            .map(String.init)
        else { return nil }
        let unquoted = token.trimmingCharacters(in: CharacterSet(charactersIn: "'\""))
        let binary = (unquoted as NSString).lastPathComponent.lowercased()
        guard !binary.isEmpty else { return nil }
        lock.lock()
        defer { lock.unlock() }
        return byBinary[binary]
    }
}

