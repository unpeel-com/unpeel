import SwiftUI
import UnpeelShared

/// Folder organize sheet, opened by long-pressing a sidebar project/group
/// row — the phone's take on the desktop project context menu: rename
/// (groups only), per-group session sort, folder color, and the archive
/// library. Saves through POST /mobile/project-organization (capability
/// `project.organization.set`); against an older Mac only the archive
/// library shows.
struct ProjectOrganizeSheet: View {
    var store: RemotePreviewStore
    let project: RemoteProjectSummary
    @Environment(\.dismiss) private var dismiss

    @State private var name: String
    @State private var colorID: String?
    @State private var dateSorted: Bool
    @State private var saving = false
    @State private var saveFailed = false

    /// Same palette ids and order as the desktop's ProjectFolderColor enum;
    /// swatches resolve through the shared IOSSidebarTheme mapping.
    private static let colorIDs = [
        "sky", "blue", "violet", "rose", "amber", "moss", "teal", "graphite",
    ]

    init(store: RemotePreviewStore, project: RemoteProjectSummary) {
        self.store = store
        self.project = project
        _name = State(initialValue: project.name)
        _colorID = State(initialValue: project.colorID)
        _dateSorted = State(initialValue: project.dateSorted ?? false)
    }

    private var canEdit: Bool {
        store.supportsProjectOrganization
    }

    /// Rename is a group verb, exactly like the desktop context menu — plain
    /// projects and worktrees keep their checkout-derived names.
    private var canRename: Bool {
        canEdit && project.isGroup == true
    }

    /// Folder color is a MAIN-project verb: groups and worktrees stay
    /// neutral so nesting reads by indent, not tint.
    private var canColor: Bool {
        canEdit && project.isGroup != true && project.parentProjectID == nil
    }

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var namePatch: String? {
        guard canRename else { return nil }
        return trimmedName.isEmpty || trimmedName == project.name ? nil : trimmedName
    }

    /// "" clears back to the default tint; nil = unchanged.
    private var colorPatch: String? {
        guard canColor else { return nil }
        return colorID == project.colorID ? nil : (colorID ?? "")
    }

    private var dateSortedPatch: Bool? {
        dateSorted == (project.dateSorted ?? false) ? nil : dateSorted
    }

    private var archivedCount: Int {
        project.archivedSessionCount ?? 0
    }

    var body: some View {
        NavigationStack {
            Form {
                if canRename {
                    Section {
                        TextField("Folder name", text: $name)
                            .submitLabel(.done)
                            .onSubmit(save)
                    } footer: {
                        if saveFailed {
                            Text("Could not update the folder. Check the connection to your Mac.")
                                .foregroundStyle(.red)
                        }
                    }
                }

                if canEdit {
                    Section("Sort sessions") {
                        Picker("Sort sessions", selection: $dateSorted) {
                            Text("Custom order").tag(false)
                            Text("Date (newest first)").tag(true)
                        }
                        .pickerStyle(.inline)
                        .labelsHidden()
                    }

                    if canColor {
                        Section("Folder color") {
                            colorRow
                        }
                    }
                }

                if archivedCount > 0 {
                    Section {
                        Button {
                            store.archiveLibraryProjectID = project.id
                            store.topBarSheet = .archive
                        } label: {
                            Label("Archived (\(archivedCount))", systemImage: "archivebox")
                        }
                    } footer: {
                        Text("Older sessions filed away on your Mac — restore or resume them anytime.")
                    }
                }

                if !canEdit, archivedCount == 0 {
                    Section {
                        Text("Update Unpeel on your Mac to organize folders from the phone.")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle(project.isGroup == true ? "Edit Group" : "Edit Folder")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .disabled(saving)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else if canEdit {
                        Button("Save", action: save)
                            .disabled(canRename && trimmedName.isEmpty)
                    }
                }
            }
        }
        .interactiveDismissDisabled(saving)
    }

    /// Default swatch + the eight tint swatches, one tap each; the selected
    /// one wears a ring. Mirrors the desktop's "Folder color" submenu.
    private var colorRow: some View {
        HStack(spacing: 10) {
            colorSwatch(id: nil)
            ForEach(Self.colorIDs, id: \.self) { id in
                colorSwatch(id: id)
            }
        }
        .padding(.vertical, 2)
    }

    private func colorSwatch(id: String?) -> some View {
        let selected = colorID == id
        let fill = id.flatMap { IOSSidebarTheme.folderColor(for: $0) }
        return Button {
            colorID = id
        } label: {
            ZStack {
                if let fill {
                    Circle().fill(fill)
                } else {
                    // "Default" — neutral swatch with a slash-free hollow look.
                    Circle().strokeBorder(.secondary, lineWidth: 1.5)
                }
            }
            .frame(width: 24, height: 24)
            .overlay(
                Circle()
                    .strokeBorder(selected ? Color.accentColor : .clear, lineWidth: 2)
                    .padding(-3)
            )
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(id.map { $0.capitalized } ?? "Default")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func save() {
        guard !saving else { return }
        guard namePatch != nil || colorPatch != nil || dateSortedPatch != nil else {
            dismiss()
            return
        }
        saving = true
        saveFailed = false
        Task {
            let ok = await store.applyProjectOrganization(
                RemoteProjectOrganizationPatch(
                    projectID: project.id,
                    displayName: namePatch,
                    colorID: colorPatch,
                    dateSorted: dateSortedPatch
                )
            )
            saving = false
            if ok {
                dismiss()
            } else {
                saveFailed = true
            }
        }
    }
}
