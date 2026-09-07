import SwiftUI

/// SwiftUI allocation/delegation wrapper for a locally rendered Surface.
///
/// `presenter` must be backed by unpeel-surface's connected USRF decoder and
/// CAMetalLayer presenter. App Kit never decodes the stream and never accepts
/// rendered frames. A Host should advertise the `surface` capability only
/// when it can supply that presenter and authorize the component reference.
@MainActor
public struct SurfaceComponentView<Presenter: View>: View {
    public let snapshot: UISnapshot
    public let viewport: SurfaceViewportSize?
    private let presenter: (SurfaceSpec) -> Presenter

    public init(
        snapshot: UISnapshot,
        viewport: SurfaceViewportSize? = nil,
        @ViewBuilder presenter: @escaping (SurfaceSpec) -> Presenter
    ) {
        self.snapshot = snapshot
        self.viewport = viewport
        self.presenter = presenter
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .surface(surface):
            presenter(surface)
                .modifier(SurfacePointFrame(surface: surface, viewport: viewport))
                .background(background(surface.background))
                .allowsHitTesting(surface.inputPolicy != .none)
                .focusable(surface.inputPolicy == .pointerAndKeyboard)
        case .canvasPage, .markdownEditor, .media, .menu, .page, .textBox, .tree, .unsupported:
            EmptyView()
        }
    }

    private func background(_ policy: SurfaceBackground) -> Color {
        switch policy {
        case .transparent:
            .clear
        case let .solid(color):
            Color(surfaceSRGBA: color) ?? .clear
        }
    }
}

struct SurfacePointFrame: ViewModifier {
    let surface: SurfaceSpec
    let viewport: SurfaceViewportSize?

    func body(content: Content) -> some View {
        if let viewport, let size = surface.resolvedPointSize(viewport: viewport) {
            content.frame(width: CGFloat(size.w), height: CGFloat(size.h))
        } else if let width = surface.points?.w {
            content
                .frame(width: CGFloat(width))
                .frame(maxHeight: .infinity)
        } else if let height = surface.points?.h {
            content
                .frame(height: CGFloat(height))
                .frame(maxWidth: .infinity)
        } else {
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

extension Color {
    init?(surfaceSRGBA value: String) {
        guard value.hasPrefix("#"), value.count == 7 || value.count == 9,
              let packed = UInt64(value.dropFirst(), radix: 16)
        else { return nil }
        let includesAlpha = value.count == 9
        let redShift = includesAlpha ? 24 : 16
        let greenShift = includesAlpha ? 16 : 8
        let blueShift = includesAlpha ? 8 : 0
        let red = Double((packed >> redShift) & 0xff) / 255
        let green = Double((packed >> greenShift) & 0xff) / 255
        let blue = Double((packed >> blueShift) & 0xff) / 255
        let alpha = includesAlpha ? Double(packed & 0xff) / 255 : 1
        self.init(.sRGB, red: red, green: green, blue: blue, opacity: alpha)
    }
}
