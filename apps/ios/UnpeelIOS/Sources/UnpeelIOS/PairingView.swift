//
//  PairingView.swift
//  UnpeelIOS
//
//  Pair this phone with a workspace running Unpeel. The workspace's
//  Settings ▸ iPhone section shows a QR with a compact pairing code
//  (RemotePairingCode; a one-time token, 5-minute TTL — legacy Macs encode
//  the payload JSON); scanning it — or pasting the same code, the simulator
//  path — exchanges it for a persistent device token via POST /mobile/pair.
//
//  A pairing IS a workspace (2026-08-23): each workspace has its own Host
//  identity and paired-device list, so this sheet's Workspaces list is the
//  paired-record list, and there is no cross-workspace switching over a
//  single connection.
//

import AVFoundation
import SwiftUI
import UnpeelShared
#if os(iOS)
import UIKit
#endif

struct PairingView: View {
    @ObservedObject var connection: RemoteConnectionStore
    /// The preview store serving the *connected* workspace — used for the
    /// live tint/relay state on the active row. The model (2026-08-23):
    /// **a pairing IS a workspace.** Every workspace has its own Host
    /// identity, so the paired list and the workspace list are the same
    /// list, and switching workspace = switching the active pairing
    /// (`RemoteConnectionStore.switchTo`). There is no cross-workspace
    /// switching over one connection.
    var store: RemotePreviewStore
    @Environment(\.dismiss) private var dismiss
    @State private var pastedPayload = ""
    @State private var pairingInFlight = false
    @State private var errorMessage: String?
    @State private var scannerPaused = false
    @State private var addingMac = false
    @State private var unpairCandidate: PairedMacRecord?
    @ObservedObject private var push = PushManager.shared

    var body: some View {
        NavigationStack {
            ZStack {
                TerminalChrome.background.ignoresSafeArea()
                ScrollView {
                    if connection.pairedMacs.isEmpty {
                        firstPairingContent
                    } else {
                        workspacesContent
                    }
                }
            }
            #if os(iOS)
            // The sheet's content is never wider than the sheet (measured:
            // the hosting scroll view's contentSize.width equals its bounds
            // at every detent and Dynamic Type size), yet the whole sheet
            // could still be dragged sideways: see RootPopGestureLock.
            .background(RootPopGestureLock())
            #endif
            .navigationTitle(navigationTitle)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .preferredColorScheme(.dark)
        }
        .confirmationDialog(
            "Forget \(unpairCandidate?.macName ?? "this workspace")?",
            isPresented: Binding(
                get: { unpairCandidate != nil },
                set: { if !$0 { unpairCandidate = nil } }
            ),
            titleVisibility: .visible,
            presenting: unpairCandidate
        ) { record in
            Button("Forget \(record.macName)", role: .destructive) {
                connection.unpair(macID: record.macID)
            }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("This phone will no longer be able to control this workspace. You can also revoke this phone in the workspace's Settings on the Mac.")
        }
        // Workspace list = little content, so a short sheet instead of full
        // height; the scanner state keeps the room it needs.
        .presentationDetents(showPairingInputs ? [.large] : [.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private var navigationTitle: String {
        connection.pairedMacs.isEmpty ? "Pair with a workspace" : "Workspaces"
    }

    /// The scanner/paste inputs show when there is nothing paired yet, or
    /// when the user explicitly asked to add another workspace.
    private var showPairingInputs: Bool {
        connection.pairedMacs.isEmpty || addingMac
    }

    // MARK: - Top level: Workspaces first

    /// First-run layout — nothing paired yet, so the sheet IS the pairing
    /// flow: brand, guidance, scanner/paste, plus the app settings sections.
    private var firstPairingContent: some View {
        VStack(spacing: 20) {
            VStack(spacing: 12) {
                UnpeelBrandLogo(size: 68)
                Text("Unpeel")
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundStyle(.white)
            }
            .frame(maxWidth: .infinity)
            .padding(.top, 8)
            unpairedGuidanceSection
            scannerSection
            pasteSection
            errorSection
            notificationSection
            securitySection
            if #available(iOS 26.0, *) {
                dictationSection
            }
            #if DEBUG
            developerSection
            #endif
        }
        .padding(20)
    }

    /// The paired layout: the Workspaces list (one row per pairing) leads,
    /// with the inline add-a-workspace flow beneath it when requested.
    /// Phone-level settings — Notifications, Security, Dictation — belong to
    /// THIS PHONE, not a workspace, so they follow below.
    private var workspacesContent: some View {
        VStack(spacing: 20) {
            workspacesSection
            if addingMac {
                addMacHeader
                scannerSection
                pasteSection
            }
            errorSection
            notificationSection
            // Phone-level like Notifications: the app lock gates opening
            // Unpeel on THIS phone, not a particular workspace.
            securitySection
            if #available(iOS 26.0, *) {
                dictationSection
            }
            #if DEBUG
            developerSection
            #endif
        }
        .padding(20)
    }

    @ViewBuilder
    private var errorSection: some View {
        if let errorMessage {
            Text(errorMessage)
                .font(.footnote.weight(.medium))
                .foregroundStyle(.red.opacity(0.9))
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var notificationSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Notifications")
                .font(.caption.weight(.semibold))
                .textCase(.uppercase)
                .foregroundStyle(.white.opacity(0.5))
                .frame(maxWidth: .infinity, alignment: .leading)

            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: push.registrationState.permissionWasDenied
                    ? "bell.slash.fill" : "bell.badge.fill")
                    .foregroundStyle(push.registrationState.permissionWasDenied
                        ? Color.orange : Color.cyan)
                Text(push.registrationState.diagnosticLabel)
                    .font(.subheadline)
                    .foregroundStyle(.white.opacity(0.82))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

            if push.registrationState.permissionWasDenied {
                Button("Open iOS Settings") { openNotificationSettings() }
                    .font(.subheadline.weight(.semibold))
                    .tint(.cyan)
            } else if push.registrationState.canRetry {
                Button("Retry registration") { push.requestAndRegister() }
                    .font(.subheadline.weight(.semibold))
                    .tint(.cyan)
            }

            Text("A ready APNs token is sent to each reachable paired workspace. "
                + "Unpeel Link then delivers needs-input and opted-in finished alerts.")
                .font(.caption)
                .foregroundStyle(.white.opacity(0.5))
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color.white.opacity(0.06))
        )
    }

    private func openNotificationSettings() {
        #if os(iOS)
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
        #endif
    }

    private var addMacHeader: some View {
        HStack {
            Text("Pair the new workspace")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.white.opacity(0.85))
            Spacer()
            Button("Cancel") { addingMac = false }
                .font(.subheadline)
                .tint(.cyan)
        }
    }

    /// App lock: require Face ID / Touch ID (with passcode fallback) to open
    /// the app. Enabling authenticates once up front so the toggle only arms
    /// when the method actually works.
    private var securitySection: some View {
        let capability = AppLockManager.capability()
        return VStack(alignment: .leading, spacing: 12) {
            Text("Security")
                .font(.caption.weight(.semibold))
                .textCase(.uppercase)
                .foregroundStyle(.white.opacity(0.5))
                .frame(maxWidth: .infinity, alignment: .leading)

            Toggle(isOn: Binding(
                get: { AppLockManager.shared.isEnabled },
                set: { enabled in
                    if enabled {
                        Task { _ = await AppLockManager.shared.enable() }
                    } else {
                        AppLockManager.shared.disable()
                    }
                }
            )) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Require \(AppLockManager.methodLabel())")
                        .foregroundStyle(.white)
                    Text("Locks Unpeel when you leave the app.")
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.5))
                }
            }
            .disabled(!capability.available)

            if !capability.available {
                Text("Set a device passcode (and enroll Face ID) in the iOS Settings app to use the app lock.")
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.5))
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color.white.opacity(0.06))
        )
    }

    /// Dictation: the optional Apple Intelligence cleanup pass over finished
    /// dictations (`DictationReflection.swift`). iOS 26+ only; on devices
    /// without Apple Intelligence the pass silently falls back to verbatim,
    /// so the toggle stays visible with the requirement noted below it.
    @available(iOS 26.0, *)
    private var dictationSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Dictation")
                .font(.caption.weight(.semibold))
                .textCase(.uppercase)
                .foregroundStyle(.white.opacity(0.5))
                .frame(maxWidth: .infinity, alignment: .leading)

            Toggle(isOn: Binding(
                get: { DictationSettings.shared.reflectionEnabled },
                set: { DictationSettings.shared.reflectionEnabled = $0 }
            )) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Polish with Apple Intelligence")
                        .foregroundStyle(.white)
                    Text("Cleans up punctuation and filler words on-device before dictated text is pasted. Requires Apple Intelligence.")
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.5))
                }
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color.white.opacity(0.06))
        )
    }

    #if DEBUG
    /// Dev-only toggles (DEBUG builds). Add flags to `DevSettings` and a Toggle
    /// here; read the flag wherever you need it.
    private var developerSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Developer")
                .font(.caption.weight(.semibold))
                .textCase(.uppercase)
                .foregroundStyle(.white.opacity(0.5))
                .frame(maxWidth: .infinity, alignment: .leading)

            Toggle(isOn: Binding(
                get: { DevSettings.shared.showTerminalBounds },
                set: { DevSettings.shared.showTerminalBounds = $0 }
            )) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Show terminal bounds")
                        .foregroundStyle(.white)
                    Text("Red outline around the terminal grid.")
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.5))
                }
            }
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color.white.opacity(0.06))
        )
    }
    #endif

    // MARK: - Sections

    /// The Workspaces list — the paired records themselves (a pairing IS a
    /// workspace): tap to switch the active connection, minus to forget,
    /// footer to pair another. Each workspace pairs individually, from
    /// inside that workspace on the Mac.
    private var workspacesSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(connection.pairedMacs, id: \.macID) { record in
                workspaceRow(record)
            }
            if !addingMac {
                Button {
                    addingMac = true
                } label: {
                    Label("Add a Workspace", systemImage: "plus.circle")
                        .font(.subheadline.weight(.semibold))
                }
                .tint(.cyan)
                .padding(.top, 4)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }

    /// First-run guidance when nothing is paired yet (dev bridge or the
    /// pairing pointer).
    @ViewBuilder
    private var unpairedGuidanceSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            if RemoteConnectionStore.devBridgeAvailable {
                Label("Using the local dev bridge (Simulator)", systemImage: "hammer")
                    .font(.subheadline)
                    .foregroundStyle(.white.opacity(0.7))
                Text("Pair with a workspace to use the real connection, or keep the dev bridge for local development.")
                    .font(.footnote)
                    .foregroundStyle(.white.opacity(0.5))
            } else {
                Label("Not paired", systemImage: "wifi.slash")
                    .font(.subheadline)
                    .foregroundStyle(.white.opacity(0.7))
                Text("Open Unpeel on your Mac, switch to the workspace you want, and show the pairing code in Settings ▸ iPhone. Each workspace pairs separately.")
                    .font(.footnote)
                    .foregroundStyle(.white.opacity(0.5))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(16)
        .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }

    /// One paired workspace: tint dot + name + accent check on the active
    /// one, endpoint (or relay) as the subtitle, minus to forget. Tapping
    /// switches the ACTIVE CONNECTION to that workspace's own Host —
    /// per-workspace pairing means there is nothing else to switch.
    private func workspaceRow(_ record: PairedMacRecord) -> some View {
        let isActive = record.macID == connection.activeMacID
        // The active row's dot follows the live bootstrap tint; others show
        // their last-known stored hue.
        let tintHue = isActive ? (store.hostTintHue ?? record.tintHue) : record.tintHue
        return HStack(spacing: 12) {
            Button {
                guard !isActive else { return }
                // Dismiss instantly — the pinned sidebar header carries the
                // connection state while the new workspace loads.
                connection.pairingSheetPresented = false
                connection.switchTo(macID: record.macID)
            } label: {
                HStack(spacing: 12) {
                    WorkspaceTintDot(tintHue: tintHue, size: 12)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(record.macName)
                            .font(isActive ? .subheadline.weight(.semibold) : .subheadline)
                            .foregroundStyle(.white)
                            .lineLimit(1)
                        Text(
                            isActive && connection.usingRelay
                                ? "via Unpeel Remote"
                                : record.endpoint.absoluteString
                        )
                        .font(.caption.monospaced())
                        .foregroundStyle(.white.opacity(0.55))
                        .lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    if isActive {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(.cyan)
                    }
                }
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(
                "Workspace \(record.macName)\(isActive ? ", current" : ""). Switch to this workspace."
            )
            Button {
                unpairCandidate = record
            } label: {
                Image(systemName: "minus.circle")
                    .foregroundStyle(.white.opacity(0.45))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Forget \(record.macName)")
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private var scannerSection: some View {
        #if os(iOS) && !targetEnvironment(simulator)
        VStack(alignment: .leading, spacing: 10) {
            Text("Scan the pairing code")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.white.opacity(0.85))
            PairingScannerView(paused: scannerPaused) { code in
                handlePayload(code)
            }
            .frame(height: 240)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        #else
        EmptyView()
        #endif
    }

    private var pasteSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Or paste the pairing code")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.white.opacity(0.85))
            TextField("Pairing code", text: $pastedPayload, axis: .vertical)
                .font(.caption.monospaced())
                .lineLimit(3 ... 5)
                .textFieldStyle(.plain)
                .autocorrectionDisabled()
                #if os(iOS)
                .textInputAutocapitalization(.never)
                #endif
                .padding(10)
                .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            Button {
                handlePayload(pastedPayload)
            } label: {
                if pairingInFlight {
                    ProgressView().frame(maxWidth: .infinity)
                } else {
                    Text("Connect")
                        .font(.subheadline.weight(.semibold))
                        .frame(maxWidth: .infinity)
                }
            }
            .buttonStyle(.borderedProminent)
            .tint(.cyan)
            .disabled(pairingInFlight || pastedPayload.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    // MARK: - Pairing

    private func handlePayload(_ raw: String) {
        guard !pairingInFlight else { return }
        guard let payload = RemotePairingCode.decode(raw) else {
            errorMessage = "That doesn't look like an Unpeel pairing code."
            return
        }
        pairingInFlight = true
        scannerPaused = true
        errorMessage = nil
        Task { @MainActor in
            do {
                try await connection.completePairing(with: payload)
                // The new Mac is upserted and active; land on it directly.
                addingMac = false
                dismiss()
            } catch let error as PairingError {
                errorMessage = error.message
            } catch {
                errorMessage = "Couldn't reach the Mac — make sure both devices are on the same network."
            }
            pairingInFlight = false
            scannerPaused = false
        }
    }
}

#if os(iOS)
/// Kills the Workspaces sheet's phantom horizontal drag. SwiftUI's
/// NavigationStack backing controller (`UIKitNavigationController`) leaves its
/// interactive pop gesture willing to begin with only the ROOT on the stack —
/// its delegate answers `gestureRecognizerShouldBegin == true` at depth 1 when
/// a `navigationDestination` is attached — so a horizontal drag on the sheet
/// grabs `_UIParallaxTransitionPanGestureRecognizer` and slides the entire
/// root content sideways over nothing ("horizontal scrolling" of the sheet).
/// The content itself is not too wide: the hosting scroll view measures
/// contentSize.width == bounds.width at every detent and type size.
///
/// The lock disables the pop recognizer while the root is frontmost and
/// re-enables it whenever the root disappears (a sub-view like Devices is
/// pushed), so the ordinary back-swipe in sub-views keeps working.
private struct RootPopGestureLock: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> LockController { LockController() }
    func updateUIViewController(_ controller: LockController, context: Context) {}

    final class LockController: UIViewController {
        private weak var lockedRecognizer: UIGestureRecognizer?

        override func viewDidAppear(_ animated: Bool) {
            super.viewDidAppear(animated)
            guard let recognizer = navigationController?.interactivePopGestureRecognizer
            else { return }
            lockedRecognizer = recognizer
            recognizer.isEnabled = false
        }

        override func viewWillDisappear(_ animated: Bool) {
            super.viewWillDisappear(animated)
            lockedRecognizer?.isEnabled = true
            lockedRecognizer = nil
        }
    }
}
#endif

// MARK: - QR scanner (device only)

#if os(iOS) && !targetEnvironment(simulator)
private struct PairingScannerView: UIViewRepresentable {
    let paused: Bool
    let onCode: (String) -> Void

    func makeUIView(context: Context) -> ScannerPreview {
        let view = ScannerPreview()
        view.onCode = { code in
            DispatchQueue.main.async { onCode(code) }
        }
        view.start()
        return view
    }

    func updateUIView(_ view: ScannerPreview, context: Context) {
        view.setScanning(enabled: !paused)
    }

    static func dismantleUIView(_ view: ScannerPreview, coordinator: ()) {
        view.stop()
    }

    final class ScannerPreview: UIView, AVCaptureMetadataOutputObjectsDelegate {
        var onCode: ((String) -> Void)?
        private var scanningEnabled = true
        private let session = AVCaptureSession()
        private var lastCode: String?

        /// Pausing stops the capture session, not just the metadata gate —
        /// otherwise the camera (and its status indicator) stays hot for the
        /// whole pairing exchange.
        func setScanning(enabled: Bool) {
            guard scanningEnabled != enabled else { return }
            scanningEnabled = enabled
            let session = session
            DispatchQueue.global(qos: .userInitiated).async {
                if enabled {
                    if !session.isRunning { session.startRunning() }
                } else if session.isRunning {
                    session.stopRunning()
                }
            }
        }

        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        private var previewLayer: AVCaptureVideoPreviewLayer {
            layer as! AVCaptureVideoPreviewLayer
        }

        func start() {
            backgroundColor = .black
            previewLayer.videoGravity = .resizeAspectFill
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                guard granted else { return }
                DispatchQueue.main.async { self?.configureSession() }
            }
        }

        func stop() {
            let session = session
            DispatchQueue.global(qos: .userInitiated).async { session.stopRunning() }
        }

        private func configureSession() {
            guard session.inputs.isEmpty,
                  let device = AVCaptureDevice.default(for: .video),
                  let input = try? AVCaptureDeviceInput(device: device),
                  session.canAddInput(input)
            else { return }
            session.addInput(input)

            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]

            previewLayer.session = session
            // If a pairing exchange paused scanning while permission/config
            // was still in flight, stay stopped; setScanning restarts later.
            guard scanningEnabled else { return }
            let session = session
            DispatchQueue.global(qos: .userInitiated).async { session.startRunning() }
        }

        nonisolated func metadataOutput(
            _: AVCaptureMetadataOutput,
            didOutput objects: [AVMetadataObject],
            from _: AVCaptureConnection
        ) {
            // Extract before the actor hop: AVMetadataObject is not Sendable.
            let code = (objects.first as? AVMetadataMachineReadableCodeObject)?.stringValue
            guard let code else { return }
            // Delegate queue is .main (set in configureSession), so this is
            // a documented-safe assume, not a hope.
            MainActor.assumeIsolated {
                guard scanningEnabled, code != lastCode else { return }
                lastCode = code
                onCode?(code)
            }
        }
    }
}
#endif
