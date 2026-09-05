//
//  ToastCenter.swift
//  UnpeelNative
//
//  A tiny transient in-app toast — the app had no message/snackbar surface, so
//  presence changes (a phone connecting) were only visible as the small viewer
//  avatar chips in the title bar. `ToastCenter.shared.show(...)` posts a brief
//  capsule at the bottom of the window; `ToastOverlayView` renders it.
//

import SwiftUI

@MainActor
final class ToastCenter: ObservableObject {
    static let shared = ToastCenter()

    struct Toast: Equatable {
        let id: UUID
        let text: String
        let systemImage: String
        /// Set → the toast leads with this app-chrome icon instead of the
        /// SF Symbol (e.g. the Phosphor globe for local-site links).
        let chromeIcon: ChromeIcon?
        /// Optional tap action (e.g. open the detected local site). A tap
        /// always dismisses; with an action it also runs it.
        let onTap: (() -> Void)?

        static func == (lhs: Toast, rhs: Toast) -> Bool { lhs.id == rhs.id }
    }

    @Published private(set) var current: Toast?
    private var dismissTask: Task<Void, Never>?

    /// Show a toast for `seconds`, replacing any current one.
    func show(
        _ text: String,
        systemImage: String = "iphone",
        chromeIcon: ChromeIcon? = nil,
        seconds: Double = 3.2,
        onTap: (() -> Void)? = nil
    ) {
        current = Toast(
            id: UUID(),
            text: text,
            systemImage: systemImage,
            chromeIcon: chromeIcon,
            onTap: onTap
        )
        dismissTask?.cancel()
        let ns = UInt64(seconds * 1_000_000_000)
        dismissTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: ns)
            guard !Task.isCancelled else { return }
            self?.current = nil
        }
    }

    func dismiss() {
        dismissTask?.cancel()
        current = nil
    }
}

/// Top-right capsule toast, just below the custom title bar. Mount once over
/// the app layout.
struct ToastOverlayView: View {
    @ObservedObject private var center = ToastCenter.shared

    var body: some View {
        VStack {
            if let toast = center.current {
                HStack(spacing: 8) {
                    if let chromeIcon = toast.chromeIcon {
                        ChromeIconView(icon: chromeIcon, size: 15)
                            .foregroundStyle(Theme.accent)
                    } else {
                        Image(systemName: toast.systemImage)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Theme.accent)
                    }
                    Text(toast.text)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.foreground)
                        .lineLimit(1)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .toastGlassBackground()
                .shadow(color: .black.opacity(0.3), radius: 16, y: 6)
                // Clear the 38px custom title bar; tuck to the trailing edge.
                .padding(.top, 46)
                .padding(.trailing, 14)
                .contentShape(Capsule())
                .onTapGesture {
                    let action = toast.onTap
                    center.dismiss()
                    action?()
                }
                .transition(.move(edge: .top).combined(with: .opacity))
                .id(toast.id)
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        .animation(.spring(response: 0.34, dampingFraction: 0.82), value: center.current)
        .allowsHitTesting(center.current != nil)
    }
}

private extension View {
    /// iOS/macOS 26 Liquid Glass capsule for the toast; regular-material
    /// fallback below 26.
    @ViewBuilder
    func toastGlassBackground() -> some View {
        if #available(macOS 26.0, *) {
            glassEffect(.regular, in: Capsule())
        } else {
            background(.regularMaterial, in: Capsule())
                .overlay(Capsule().strokeBorder(.white.opacity(0.12)))
        }
    }
}
