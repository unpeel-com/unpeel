//
//  SidebarWorkspaceSelector.swift
//  UnpeelNative
//
//  Workspaces unification: local
//  workspaces and remote Hosts are one product concept. The sidebar footer
//  dots are the compact switcher; secondary-clicking them opens this picker.
//  The picker contains the default instance,
//  registry workspaces from ~/.unpeel/profiles.json,
//  paired Hosts, and SSH Hosts. Clicking opens the picker popover (rows
//  with tint dots/tags/rename + Manage). Remote selection routes through
//  the existing selectHost scope path; another local workspace rescopes
//  this window in place over the phase-2 loopback gateway
//  (selectLocalWorkspace), exactly like a remote Host. The terminal header
//  remains the ordinary project/group breadcrumb.
//
//  Local workspace switching ships whenever Workspaces is enabled. Paired
//  and SSH rows remain behind RemoteHostFeature.pickerEnabled (Unpeel Dev).
//

import AppKit
import SwiftUI

struct SidebarWorkspacePopover: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var hosts: RemoteHostStore
    @ObservedObject var pool: WorkspacePool

    @State private var pickerPresented = false

    var body: some View {
        if WorkspaceFeature.pickerEnabled {
            WorkspaceSecondaryClickAnchor(action: presentPicker)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                // Attach to the top edge of the footer strip so the popover
                // opens above the workspace dots, inside the app window.
                .popover(
                    isPresented: $pickerPresented,
                    attachmentAnchor: .rect(.bounds),
                    arrowEdge: .top
                ) {
                    WorkspacePickerPanel(
                        store: store,
                        hosts: hosts,
                        pool: pool,
                        dismiss: { pickerPresented = false }
                    )
                }
        }
    }

    private func presentPicker() {
        // Opening the picker is a pool immediate-refresh trigger (throttled
        // inside) so attention badges and later peeks are fresh.
        pool.requestImmediateRefresh()
        pickerPresented = true
    }
}

/// The shared workspace picker: one flat list of every workspace (remote
/// rows tagged, the shared user order), rename in place, and the Manage
/// footer. Presented by BOTH anchors — the sidebar footer dots' secondary
/// click and the Settings sidebar's scope dropdown — so the two entry
/// points can never drift.
struct WorkspacePickerPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var hosts: RemoteHostStore
    /// Background workspace pool: attention badges on rows whose workspace
    /// has a session needing input.
    @ObservedObject var pool: WorkspacePool
    /// Close the presenting popover (a row was chosen or Manage opened).
    let dismiss: () -> Void

    /// Registry + liveness + tints are disk-backed, so the unified rows are
    /// cached here and refreshed on appearance — never read in a body
    /// evaluation. The sidebar body re-runs constantly with many sessions;
    /// this surface must stay free.
    @State private var rows: [WorkspaceListRowModel] = []
    @State private var errorMessage: String?
    @State private var manageHovering = false
    @State private var renameTarget: WorkspaceRenameTarget?
    @State private var renameText = ""
    @FocusState private var renameFocused: Bool

    var body: some View {
        pickerContent
            .onAppear(perform: refresh)
            // A per-line color change (this or any workspace, in Settings
            // ▸ Workspaces, or a peer's cross-instance ping) refreshes the
            // dots now, not just on the next open. Fired once per change —
            // unlike `unpeelAppTintChanged`, which repeats through the fade.
            .onReceive(
                NotificationCenter.default.publisher(for: .unpeelWorkspaceTintChanged)
            ) { _ in refresh() }
    }

    @ViewBuilder
    private var pickerContent: some View {
        VStack(alignment: .leading, spacing: 4) {
            // One flat list, local and remote together — remote rows are
            // tagged instead of sectioned, and the order is the shared
            // user order (drag to reorder in Settings ▸ Workspaces).
            ForEach(rows) { row in
                entry(row)
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.system(size: 11))
                    .foregroundStyle(.red.opacity(0.9))
                    .padding(.horizontal, 7)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Divider().padding(.vertical, 3)
            manageRow
        }
        .padding(8)
        .frame(width: 300)
    }

    /// A tap-gesture row rather than a Button: a plain Button here draws a
    /// keyboard focus ring inside the popover (the picker rows above avoid it
    /// the same way).
    private var manageRow: some View {
        HStack(spacing: 8) {
            Image(systemName: "gearshape")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .frame(width: 18)
            Text("Manage Workspaces…")
                .font(Theme.rowLabelFont)
                .foregroundStyle(Theme.foreground)
            Spacer(minLength: 8)
        }
        .padding(.horizontal, 7)
        .frame(height: 30)
        .contentShape(Rectangle())
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(manageHovering ? Theme.hoverRow : .clear)
        )
        .onHover { manageHovering = $0 }
        .onTapGesture {
            dismiss()
            store.openSettings(tab: .workspaces)
        }
    }

    @ViewBuilder
    private func entry(_ row: WorkspaceListRowModel) -> some View {
        let target = renameTargetFor(row)
        if let target, renameTarget == target {
            renameEditor(target: target)
        } else {
            WorkspacePickerRow(
                title: row.name,
                quietTag: quietTagFor(row),
                tag: tagFor(row),
                color: row.tint.nsSwatch,
                selected: isChecked(row),
                // Background-workspace attention from the pool; the scoped
                // row shows its own per-session state instead.
                attention: !isChecked(row) && pool.attentionKeys.contains(row.id),
                select: {
                    dismiss()
                    select(row)
                },
                rename: target.map { target in
                    { beginRename(target, currentName: row.name) }
                },
                openInNewWindow: openInNewWindowAction(row)
            )
        }
    }

    private func tagFor(_ row: WorkspaceListRowModel) -> String? {
        switch row.kind {
        case let .local(_, _, isCurrentInstance, isRunning):
            return isRunning && !isCurrentInstance ? "Running" : nil
        case .paired, .ssh:
            return "Remote"
        }
    }

    /// Passive metadata, plainer than the capsule state tags: the default
    /// workspace (whose appearance becomes the inherited baseline) is worth
    /// knowing, not worth a badge.
    private func quietTagFor(_ row: WorkspaceListRowModel) -> String? {
        if case .local(_, true, _, _) = row.kind {
            return "Default"
        }
        return nil
    }

    /// "Unpin" a LOCAL workspace into its own window — today that window is
    /// its own app instance (`UnpeelWorkspaceLauncher`, the tear-out seam's
    /// sanctioned initial path). A running instance is asked to show its
    /// window instead of double-launching; remote Hosts have no local window
    /// and this instance's own row is already this window.
    private func openInNewWindowAction(_ row: WorkspaceListRowModel) -> (() -> Void)? {
        guard case let .local(record, isDefault, isCurrentInstance, _) = row.kind,
              !isCurrentInstance
        else { return nil }
        let launchRecord: UnpeelWorkspaceRecord
        if let record {
            launchRecord = record
        } else if isDefault {
            // The default workspace has no registry record; the launcher
            // only needs its home and a display name.
            launchRecord = UnpeelWorkspaceRecord(
                id: "default",
                name: row.name,
                home: UnpeelWorkspaceRegistry.realUnpeelDir.path,
                createdAt: 0
            )
        } else {
            return nil
        }
        return {
            errorMessage = nil
            let home = URL(fileURLWithPath: launchRecord.home, isDirectory: true)
            if UnpeelWorkspaceLauncher.runningPid(home: home) != nil {
                UnpeelWorkspaceLauncher.showWindow(home: home)
                dismiss()
                return
            }
            do {
                try UnpeelWorkspaceLauncher.launch(launchRecord)
                dismiss()
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    private func renameTargetFor(_ row: WorkspaceListRowModel) -> WorkspaceRenameTarget? {
        switch row.kind {
        case let .local(record, isDefault, _, _):
            if let record { return .local(record.id) }
            return isDefault ? .defaultWorkspace : nil
        case .paired(let host):
            return .remote(.paired, host.hostID)
        case .ssh(let host):
            return .remote(.ssh, host.id)
        }
    }

    private func isChecked(_ row: WorkspaceListRowModel) -> Bool {
        WorkspaceSwitching.isScoped(row, store: store)
    }

    /// Shared switch semantics (WorkspaceSwitching): current instance back to
    /// local scope, another local workspace over the loopback gateway,
    /// paired/SSH Hosts through selectHost — via the single-slide pager
    /// (selectSliding), which falls back to the instant switch when the
    /// pager surface is unavailable.
    private func select(_ row: WorkspaceListRowModel) {
        errorMessage = nil
        WorkspaceSwitching.selectSliding(row, store: store)
    }

    private func renameEditor(target: WorkspaceRenameTarget) -> some View {
        HStack(spacing: 6) {
            TextField("Name", text: $renameText)
                .textFieldStyle(.roundedBorder)
                .focused($renameFocused)
                .onSubmit { commitRename(target) }
            Button {
                commitRename(target)
            } label: {
                Image(systemName: "checkmark")
            }
            .buttonStyle(.borderless)
            .disabled(renameText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            Button {
                cancelRename()
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.borderless)
        }
        .padding(.horizontal, 7)
        .frame(height: 30)
    }

    // MARK: - Actions

    private func beginRename(_ target: WorkspaceRenameTarget, currentName: String) {
        errorMessage = nil
        renameText = currentName
        renameTarget = target
        DispatchQueue.main.async { renameFocused = true }
    }

    private func cancelRename() {
        renameFocused = false
        renameTarget = nil
        renameText = ""
    }

    private func commitRename(_ target: WorkspaceRenameTarget) {
        let name = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        let renamed: Bool
        switch target {
        case .defaultWorkspace:
            renamed = UnpeelWorkspaceContext.renameDefaultWorkspace(to: name)
        case .local(let id):
            UnpeelWorkspaceRegistry.rename(id: id, to: name)
            renamed = true
        case .remote(_, let id):
            renamed = hosts.renameHost(id, to: name)
        }
        guard renamed else {
            errorMessage = "That workspace is no longer available."
            return
        }
        cancelRename()
        refresh()
        NotificationCenter.default.post(name: .unpeelWorkspaceListChanged, object: nil)
    }

    // MARK: - Cache refresh

    private func refresh() {
        // Same rows and saved user order as Settings ▸ Workspaces (reorder
        // lives there); the selector always lists every workspace. Each row's
        // `.tint` is read fresh here from that workspace's own defaults suite,
        // so dots stay in sync with the settings-screen pickers.
        rows = WorkspaceSwitching.orderedRows(store: store)
    }
}

/// Transparent popover anchor layered behind the footer dots. A local event
/// monitor observes only secondary clicks inside this view's bounds, leaving
/// the dot Buttons' ordinary primary-click behavior completely untouched.
private struct WorkspaceSecondaryClickAnchor: NSViewRepresentable {
    let action: () -> Void

    func makeNSView(context _: Context) -> MonitorView {
        MonitorView(action: action)
    }

    func updateNSView(_ nsView: MonitorView, context _: Context) {
        nsView.action = action
    }

    static func dismantleNSView(_ nsView: MonitorView, coordinator _: ()) {
        nsView.removeMonitor()
    }

    final class MonitorView: NSView {
        var action: () -> Void
        private var eventMonitor: Any?

        init(action: @escaping () -> Void) {
            self.action = action
            super.init(frame: .zero)
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            removeMonitor()
            guard window != nil else { return }
            eventMonitor = NSEvent.addLocalMonitorForEvents(matching: .rightMouseDown) {
                [weak self] event in
                guard let self,
                      event.window === self.window,
                      self.bounds.contains(self.convert(event.locationInWindow, from: nil))
                else { return event }
                self.action()
                return nil
            }
        }

        override func hitTest(_: NSPoint) -> NSView? { nil }

        func removeMonitor() {
            guard let eventMonitor else { return }
            NSEvent.removeMonitor(eventMonitor)
            self.eventMonitor = nil
        }
    }
}

private enum WorkspaceRenameTarget: Equatable {
    enum RemoteKind: Equatable {
        case paired
        case ssh
    }

    case defaultWorkspace
    case local(String)
    case remote(RemoteKind, String)
}

/// A real popover row rather than an NSMenu item, so secondary click and a
/// double click can both expose per-workspace actions. The exclusive gesture
/// prevents the first click of a double click from selecting and dismissing
/// the popover before rename begins.
private struct WorkspacePickerRow: View {
    let title: String
    /// Plain muted text after the title (e.g. "Default") — metadata, not a
    /// state capsule like `tag`.
    var quietTag: String? = nil
    var tag: String? = nil
    let color: NSColor
    let selected: Bool
    var attention = false
    let select: () -> Void
    let rename: (() -> Void)?
    var openInNewWindow: (() -> Void)? = nil

    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            Image(nsImage: WorkspaceMenuSwatch.image(color: color, selected: selected))
                .renderingMode(.original)
            Text(title)
                .font(Theme.rowLabelFont)
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
            if let quietTag {
                Text(quietTag)
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.mutedForeground)
            }
            if attention {
                // A session in this background workspace needs input.
                Circle()
                    .fill(Theme.attention)
                    .frame(width: 5, height: 5)
            }
            Spacer(minLength: 8)
            if let tag {
                Text(tag)
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(Theme.mutedForeground)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(Theme.mutedForeground.opacity(0.14), in: Capsule())
            }
        }
        .padding(.horizontal, 7)
        .frame(height: 30)
        .contentShape(Rectangle())
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(hovering ? Theme.hoverRow : .clear)
        )
        .onHover { hovering = $0 }
        .gesture(
            TapGesture(count: 2)
                .exclusively(before: TapGesture(count: 1))
                .onEnded { value in
                    switch value {
                    case .first(_): (rename ?? select)()
                    case .second(_): select()
                    }
                }
        )
        .contextMenu {
            if let openInNewWindow {
                Button("Open in New Window", action: openInNewWindow)
            }
            if let rename {
                Button("Rename…", action: rename)
            }
        }
        .help(rename == nil ? title : "Double-click or right-click to rename")
        .accessibilityLabel(title)
    }
}

extension AppTint {
    /// Menu-icon flavor of `swatch`. NSMenu flattens custom views, so the dot
    /// is drawn into an NSImage; that bakes one color, hence a single
    /// mid-gray stand-in for the appearance-dynamic neutral swatch.
    var nsSwatch: NSColor {
        switch self {
        case .none: return NSColor(hex: 0x8A8F99)
        case .peel: return NSColor(hex: 0xD97757)
        case .amber: return NSColor(hex: 0xE3A63B)
        case .green: return NSColor(hex: 0x3FBF63)
        case .teal: return NSColor(hex: 0x4EC3C9)
        case .blue: return NSColor(hex: 0x4FA8FF)
        case .indigo: return NSColor(hex: 0x7A7EF2)
        case .violet: return NSColor(hex: 0xB166E8)
        }
    }
}

/// Round tint dot for workspace menu items, with the selection check drawn
/// into the image (NSMenu has no separate SwiftUI state column). Cached per
/// (color, selected) like FolderColorMenuSwatch: a fresh NSImage per menu
/// re-evaluation would give items a new identity and blink an open menu.
@MainActor
private enum WorkspaceMenuSwatch {
    private static var cache: [String: NSImage] = [:]

    static func image(color: NSColor, selected: Bool) -> NSImage {
        let rgb = color.usingColorSpace(.sRGB) ?? color
        let key = String(
            format: "%.3f-%.3f-%.3f-%@",
            rgb.redComponent, rgb.greenComponent, rgb.blueComponent,
            selected ? "on" : "off"
        )
        if let cached = cache[key] { return cached }
        let image = draw(color: color, selected: selected)
        cache[key] = image
        return image
    }

    private static func draw(color: NSColor, selected: Bool) -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18))
        image.lockFocus()
        defer { image.unlockFocus() }

        NSGraphicsContext.current?.imageInterpolation = .high

        let dot = NSBezierPath(
            ovalIn: NSRect(x: 3, y: 3, width: 12, height: 12)
        )
        color.withAlphaComponent(0.94).setFill()
        dot.fill()
        NSColor.white.withAlphaComponent(0.5).setStroke()
        dot.lineWidth = 1
        dot.stroke()

        if selected {
            let check = NSBezierPath()
            check.move(to: NSPoint(x: 6.2, y: 9.0))
            check.line(to: NSPoint(x: 8.1, y: 7.0))
            check.line(to: NSPoint(x: 12.0, y: 11.4))
            check.lineWidth = 1.8
            check.lineCapStyle = .round
            check.lineJoinStyle = .round
            NSColor.white.setStroke()
            check.stroke()
        }

        image.isTemplate = false
        return image
    }
}
