//
//  LicenseSettingsPanel.swift
//  UnpeelNative
//
//  Settings ▸ Remote ▸ Unpeel Link: paste a key, activate this Host, or
//  release the seat. The app is free; a license unlocks Unpeel Link — the
//  device-linking features (Unpeel Remote, iPhone pairing, workspaces).
//  Customer-facing "Pro" is renamed Unpeel Link (2026-08-12), and the
//  standalone license tab was merged into the Remote panel (2026-08-13) —
//  these are the Form sections it embeds there, above the enrollment list.
//  LicenseManager, the key format, and seat activation are unchanged
//  underneath (offline signature check + online activation). See
//  LicenseManager.swift.
//

import AppKit
import SwiftUI

struct LinkLicenseSections: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var license = LicenseManager.shared
    @State private var keyInput = ""
    @State private var nameInput = ""
    @FocusState private var nameFieldFocused: Bool

    var body: some View {
        switch license.state {
        // Presence (nickname + avatar) is hidden until the presence feature
        // actually ships — the profileSection stays for that day.
        case let .active(payload):
            activeSection(payload)
        case let .revoked(payload):
            revokedSection(payload)
        case .unlicensed:
            freeSection
        }
    }

    // MARK: - Presence profile (nickname + avatar)

    /// The TUI's `LINK_AVATARS` — keep the two pickers offering the same set.
    static let avatarChoices = ["🦊", "🐙", "🦉", "🐢", "🐝", "🦁", "🐬", "🌵"]

    /// How this person appears to other controllers connected to the same
    /// Host (presence chips, "who is typing"). Stored in shared
    /// app-state.json beside the TUI's identical rows; the Link control
    /// plane carries it later.
    private var profileSection: some View {
        Section {
            LabeledContent {
                TextField("Your name", text: $nameInput)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 13))
                    .frame(maxWidth: 220)
                    .focused($nameFieldFocused)
                    .onSubmit { store.setProfileDisplayName(nameInput) }
            } label: {
                Text("Display name")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
            LabeledContent {
                HStack(spacing: 4) {
                    ForEach(Self.avatarChoices, id: \.self) { emoji in
                        Button {
                            store.setProfileAvatar(emoji)
                        } label: {
                            Text(emoji)
                                .font(.system(size: 15))
                                .frame(width: 26, height: 26)
                                .background(
                                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                                        .fill(emoji == store.profileAvatar
                                            ? Theme.accent.opacity(0.25)
                                            : Color.clear)
                                )
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Avatar \(emoji)")
                    }
                }
            } label: {
                Text("Avatar")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
        } header: {
            SettingsSectionHeader(
                title: "Presence",
                description: "How you appear to others connected to the same Host — "
                    + "the name and avatar on presence chips."
            )
        }
        .onAppear { nameInput = store.profileDisplayName }
        .onChange(of: store.profileDisplayName) { external in
            // A TUI edit landed; don't clobber typing in progress.
            if !nameFieldFocused { nameInput = external }
        }
        .onChange(of: nameFieldFocused) { focused in
            if !focused { store.setProfileDisplayName(nameInput) }
        }
    }

    // MARK: - Pro active

    private func activeSection(_ payload: LicensePayload) -> some View {
        Section {
            HStack(spacing: 8) {
                Image(systemName: "checkmark.seal.fill")
                    .foregroundStyle(Theme.accent)
                Text("Unpeel Link is active")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.foreground)
            }
            SettingsValueRow(label: "Licensed to", value: payload.email)
            SettingsValueRow(label: "Seats", value: "\(payload.seats) Hosts")

            LabeledContent {
                Button(license.working ? "Deactivating…" : "Deactivate this Host") {
                    Task { await license.deactivate() }
                }
                .controlSize(.small)
                .disabled(license.working)
            } label: {
                Text("Release seat")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
            lastErrorText
        } header: {
            SettingsSectionHeader(
                title: "Unpeel Link subscription",
                description: "One compatibility seat activates one Host. Deactivating frees this Host's "
                    + "seat so you can use the key on a different machine."
            )
        }
    }

    // MARK: - Revoked

    private func revokedSection(_ payload: LicensePayload) -> some View {
        Section {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(Theme.danger)
                Text("This Unpeel Link key is no longer active")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.foreground)
            }
            Text("The subscription for \(payload.email) has ended or been refunded. Unpeel "
                + "keeps working — renew or enter a different key to use Unpeel Link again.")
                .font(.system(size: 12))
                .foregroundStyle(Theme.mutedForeground)
                .fixedSize(horizontal: false, vertical: true)
            keyField
            activateButton
            lastErrorText
        } header: {
            SettingsSectionHeader(title: "Unpeel Link subscription")
        } footer: {
            activationFooter
        }
    }

    // MARK: - Free (no key)

    private var freeSection: some View {
        Section {
            ForEach(Self.proFeatures, id: \.title) { feature in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Image(systemName: feature.icon)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.accent)
                        .frame(width: 16)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(feature.title)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(Theme.foreground)
                        Text(feature.detail)
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(.vertical, 1)
            }
            keyField
            activateButton
            lastErrorText
        } header: {
            SettingsSectionHeader(
                title: "Get Unpeel Link",
                description: "Unpeel is free on your Mac; Link is the relay above — "
                    + "$59 per seat per year, one compatibility seat activates one Host. Paste the key "
                    + "from your purchase email to activate."
            )
        } footer: {
            activationFooter
        }
    }

    private static let proFeatures: [(icon: String, title: String, detail: String)] = [
        (
            "antenna.radiowaves.left.and.right",
            "Unpeel Remote",
            "Reach your Macs from anywhere over an end-to-end encrypted relay."
        ),
        (
            "iphone",
            "iPhone remote control",
            "Pair the Unpeel iOS app to steer sessions from your phone."
        ),
        (
            "person.2",
            "Workspaces",
            "Run separate Unpeel instances with their own sessions and settings."
        )
    ]

    @ViewBuilder
    private var lastErrorText: some View {
        if let error = license.lastError {
            Text(error)
                .font(.system(size: 12))
                .foregroundStyle(Theme.danger)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var activationFooter: some View {
        HStack(spacing: 4) {
            Link("Get Unpeel Link", destination: LicenseConfig.apiBaseURL.appendingPathComponent("link"))
            Text("·").foregroundStyle(Theme.mutedForeground)
            Link("Lost your key?",
                 destination: LicenseConfig.apiBaseURL.appendingPathComponent("license/recover"))
        }
        .font(.system(size: 12))
        .padding(.top, 4)
    }

    private var keyField: some View {
        LicenseKeyTextView(
            text: $keyInput,
            placeholder: "CLRTY-…",
            isEditable: !license.working
        )
        .frame(height: 76)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(Theme.foreground.opacity(0.05))
        )
        .opacity(license.working ? 0.55 : 1)
    }

    private var activateButton: some View {
        Button(license.working ? "Activating…" : "Activate") {
            Task { await license.activate(keyInput) }
        }
        .controlSize(.regular)
        .disabled(license.working || keyInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }
}

private struct LicenseKeyTextView: NSViewRepresentable {
    @Binding var text: String
    let placeholder: String
    let isEditable: Bool

    func makeCoordinator() -> Coordinator {
        Coordinator(text: $text)
    }

    func makeNSView(context: Context) -> PlaceholderTextView {
        let textView = PlaceholderTextView()
        textView.delegate = context.coordinator
        textView.placeholder = placeholder
        textView.string = text
        textView.isEditable = isEditable
        textView.isSelectable = true
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.drawsBackground = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isContinuousSpellCheckingEnabled = false
        textView.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        textView.textColor = Theme.dynamicNSColor(
            light: NSColor(hex: 0x111217),
            dark: NSColor(hex: 0xF3F5FB)
        )
        textView.insertionPointColor = NSColor.controlAccentColor
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.textContainer?.lineFragmentPadding = 0
        textView.textContainer?.widthTracksTextView = true
        textView.textContainer?.containerSize = NSSize(
            width: 0,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.isHorizontallyResizable = false
        textView.isVerticallyResizable = true
        textView.autoresizingMask = [.width]
        textView.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return textView
    }

    func updateNSView(_ textView: PlaceholderTextView, context _: Context) {
        if textView.string != text {
            textView.string = text
        }
        textView.placeholder = placeholder
        textView.isEditable = isEditable
        textView.needsDisplay = true
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        @Binding var text: String

        init(text: Binding<String>) {
            _text = text
        }

        func textDidChange(_ notification: Notification) {
            guard let textView = notification.object as? NSTextView else { return }
            text = textView.string
            textView.needsDisplay = true
        }
    }

    final class PlaceholderTextView: NSTextView {
        var placeholder = ""

        override var isFlipped: Bool { true }

        override func draw(_ dirtyRect: NSRect) {
            super.draw(dirtyRect)
            guard string.isEmpty, !placeholder.isEmpty else { return }
            let attributes: [NSAttributedString.Key: Any] = [
                .font: NSFont.monospacedSystemFont(ofSize: 12, weight: .regular),
                .foregroundColor: Theme.dynamicNSColor(
                    light: NSColor(hex: 0x111217, opacity: 0.48),
                    dark: NSColor(hex: 0xF3F5FB, opacity: 0.48)
                ),
            ]
            placeholder.draw(
                at: CGPoint(x: textContainerInset.width, y: textContainerInset.height),
                withAttributes: attributes
            )
        }
    }
}
