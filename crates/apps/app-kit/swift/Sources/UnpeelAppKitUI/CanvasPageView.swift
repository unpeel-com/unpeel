import SwiftUI

/// Native interpretation of the closed CanvasPage composition.
///
/// The presenter closure owns only the local Surface GPU box. This view owns
/// the fixed top toolbar and emits its Button actions over the semantic UI
/// transport, keeping the scene and component protocols independent.
@MainActor
public struct CanvasPageView<Presenter: View>: View {
    public let snapshot: UISnapshot
    public let viewport: SurfaceViewportSize?
    public let onAction: (UIAction) -> Void
    private let presenter: (SurfaceSpec) -> Presenter

    public init(
        snapshot: UISnapshot,
        viewport: SurfaceViewportSize? = nil,
        onAction: @escaping (UIAction) -> Void,
        @ViewBuilder presenter: @escaping (SurfaceSpec) -> Presenter
    ) {
        self.snapshot = snapshot
        self.viewport = viewport
        self.onAction = onAction
        self.presenter = presenter
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .canvasPage(page):
            if page.requiredCapabilities != nil {
                ZStack(alignment: .topLeading) {
                    presenter(page.surface.surface)
                        .modifier(SurfacePointFrame(
                            surface: page.surface.surface,
                            viewport: viewport
                        ))
                        .background(background(page.surface.surface.background))
                        .allowsHitTesting(page.surface.surface.inputPolicy != .none)
                        .focusable(page.surface.surface.inputPolicy == .pointerAndKeyboard)

                    toolbar(page)
                        .padding(12)
                }
                .accessibilityElement(children: .contain)
                .accessibilityLabel(page.title)
            } else {
                EmptyView()
            }
        case .markdownEditor, .media, .menu, .page, .surface, .textBox, .tree, .unsupported:
            EmptyView()
        }
    }

    private func toolbar(_ page: CanvasPageSpec) -> some View {
        HStack(spacing: 8) {
            Text(page.title)
                .font(.headline)
                .lineLimit(1)
            Spacer(minLength: 16)
            ForEach(buttons(page.controls)) { button in
                control(button)
            }
        }
        .padding(8)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 11))
        .overlay {
            RoundedRectangle(cornerRadius: 11)
                .stroke(Color.white.opacity(0.16), lineWidth: 1)
        }
    }

    @ViewBuilder
    private func control(_ button: UIButtonSpec) -> some View {
        let action = Button(button.label) {
            onAction(UIAction(
                nodeID: button.id,
                action: button.action,
                kind: .activate
            ))
        }
        switch button.role {
        case .standard:
            action.buttonStyle(.bordered)
        case .primary:
            action.buttonStyle(.borderedProminent)
        case .destructive:
            action.buttonStyle(.bordered).tint(.red)
        }
    }

    private func buttons(_ controls: [UICanvasControl]) -> [UIButtonSpec] {
        controls.compactMap { control in
            guard case let .button(button) = control else { return nil }
            return button
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
