//
//  ToolIcons.swift
//  UnpeelNative
//
//  Provider artwork is owned by each `runtimes/<slug>` package and embedded
//  into UnpeelShared's generated runtime catalog. This renderer stays generic:
//  monochrome assets are NSImage templates, while authored multi-color marks
//  (currently OpenCode) preserve their fills.
//

import AppKit
import SwiftUI
import UnpeelShared

enum ToolIcons {
    @MainActor private static var cache: [String: NSImage] = [:]

    @MainActor
    static func image(for icon: UnpeelToolIcon) -> NSImage? {
        if let cached = cache[icon.id] { return cached }
        guard let image = NSImage(data: Data(icon.svgSource.utf8)) else { return nil }
        image.isTemplate = icon.isTemplate
        cache[icon.id] = image
        return image
    }
}

/// Runtime logo for preset and Session presentation. A new descriptor-provided
/// SVG flows through the generated catalog without adding a native enum case.
struct ToolIconView: View {
    let icon: UnpeelToolIcon
    let displayName: String
    let colorCommand: String
    var size: CGFloat = 14

    init(tool: QuickPresetTool?, size: CGFloat = 14) {
        let runtime = tool?.metadata
        icon = runtime.map(UnpeelToolIcon.forRuntime) ?? .terminal
        displayName = runtime?.label ?? "Terminal"
        colorCommand = runtime?.presentationCommand ?? ""
        self.size = size
    }

    init(command: String, size: CGFloat = 14) {
        let runtime = UnpeelRuntimeCatalog.runtime(command: command)
        icon = runtime.map(UnpeelToolIcon.forRuntime) ?? .terminal
        displayName = runtime?.label ?? "Terminal"
        colorCommand = runtime?.presentationCommand ?? ""
        self.size = size
    }

    var body: some View {
        if let image = resolvedImage {
            Image(nsImage: image)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .frame(width: size, height: size)
        } else {
            monogram
        }
    }

    @MainActor private var resolvedImage: NSImage? {
        ToolIcons.image(for: icon)
    }

    private var monogram: some View {
        Circle()
            .fill(Theme.toolColor(forCommand: colorCommand).opacity(0.85))
            .frame(width: size, height: size)
            .overlay(
                Text(String(displayName.prefix(1)))
                    .font(.system(size: size * 0.62, weight: .bold))
                    .foregroundStyle(Color.black.opacity(0.8))
            )
    }
}
