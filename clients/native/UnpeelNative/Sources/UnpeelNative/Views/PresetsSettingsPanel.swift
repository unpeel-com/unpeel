import SwiftUI

// Shared native settings button styles.
struct EditorButton: View {
    enum Variant { case primary, secondary, danger }
    enum Size { case regular, small }

    let title: String
    var variant: Variant = .secondary
    var size: Size = .regular
    var disabled = false
    let action: () -> Void

    var body: some View {
        styled(
            Button(title, role: variant == .danger ? .destructive : nil, action: action)
        )
        .controlSize(size == .small ? .small : .regular)
        .disabled(disabled)
    }

    @ViewBuilder
    private func styled(_ button: some View) -> some View {
        if #available(macOS 26.0, *) {
            switch variant {
            // Neutral-gray prominent capsule: keep the native glass
            // material, tint it the app gray instead of system blue.
            // glassProminent derives the label color from the tint.
            case .primary: button.buttonStyle(.glassProminent).tint(Theme.ctaTint)
            case .secondary, .danger: button.buttonStyle(.bordered)
            }
        } else {
            switch variant {
            case .primary: button.buttonStyle(.borderedProminent).tint(Theme.ctaTint)
            case .secondary, .danger: button.buttonStyle(.bordered)
            }
        }
    }
}
