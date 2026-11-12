import SwiftUI

struct RemoteFolderPage: Decodable, Sendable {
    struct Entry: Decodable, Identifiable, Sendable {
        var id: String { path }
        let name: String
        let path: String
    }
    let path: String
    let parent: String?
    let next: String?
    let entries: [Entry]
}

/// Folder selection stays in the normal launcher flow; every filesystem
/// operation runs on the selected Host and is bound to that Host's identity.
struct RemoteFolderPicker: View {
    @ObservedObject var runtime: RemoteHostRuntime
    let complete: (String?) -> Void
    @State private var path = "~"
    @State private var page: RemoteFolderPage?
    @State private var entries: [RemoteFolderPage.Entry] = []
    @State private var error: String?
    @State private var busy = false
    @State private var showHidden = false
    @State private var newFolderName = ""
    @State private var showNewFolder = false
    @State private var requestTask: Task<Void, Never>?
    @State private var hostID: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.t("Select project folder")).font(.headline)
            HStack {
                Button { if let parent = page?.parent { load(parent) } } label: {
                    Image(systemName: "arrow.up")
                }.disabled(busy || page?.parent == nil).accessibilityLabel(L10n.t("Parent folder"))
                TextField(L10n.t("Folder path"), text: $path).onSubmit { load(path) }
                Button(L10n.t("Go")) { load(path) }.disabled(busy)
            }
            List(entries) { entry in
                Button { load(entry.path) } label: {
                    Label(entry.name, systemImage: "folder").frame(maxWidth: .infinity, alignment: .leading)
                }.buttonStyle(.plain)
            }
            .overlay { if busy { ProgressView().controlSize(.small) } }
            if let error { Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Toggle(L10n.t("Show hidden folders"), isOn: $showHidden)
                    .onChange(of: showHidden) { _ in load(page?.path ?? path) }
                Spacer()
                if page?.next != nil { Button(L10n.t("More")) { load(page?.path ?? path, more: true) }.disabled(busy) }
                Button(L10n.t("New Folder")) { showNewFolder = true }
                    .disabled(busy || page == nil || !runtime.supportsHostOperation("filesystem.directories.create"))
            }
            if showNewFolder {
                HStack {
                    TextField(L10n.t("Folder name"), text: $newFolderName)
                    Button(L10n.t("Create")) { createFolder() }
                        .disabled(busy || newFolderName.isEmpty || newFolderName.contains("/") || newFolderName == "." || newFolderName == "..")
                }
            }
            HStack {
                Spacer()
                Button(L10n.t("Cancel")) { complete(nil) }.keyboardShortcut(.cancelAction)
                Button(L10n.t("Add Project")) { if let page { complete(page.path) } }
                    .keyboardShortcut(.defaultAction).disabled(busy || page == nil || error != nil)
            }
        }
        .padding(20).frame(width: 540, height: 440)
        .onAppear { hostID = runtime.snapshot?.macID; load(path) }
        .onChange(of: runtime.snapshot?.macID) { newID in if newID != hostID { complete(nil) } }
        .onDisappear { requestTask?.cancel() }
    }

    private func load(_ target: String, more: Bool = false) {
        requestTask?.cancel()
        busy = true; error = nil
        let after = more ? page?.next ?? "" : ""
        requestTask = Task {
            do {
                let data = try await runtime.resourceRequest(operation: "directories", capability: "filesystem.directories.list",
                    parameters: ["path": target, "after": after, "hidden": showHidden ? "true" : "false"])
                try Task.checkCancellation()
                let next = try JSONDecoder().decode(RemoteFolderPage.self, from: data)
                page = next; path = next.path
                entries = more ? entries + next.entries : next.entries
                busy = false
            } catch is CancellationError {} catch {
                self.error = error.localizedDescription; busy = false
            }
        }
    }

    private func createFolder() {
        guard let page else { return }
        let target = page.path.hasSuffix("/") ? page.path + newFolderName : page.path + "/" + newFolderName
        busy = true; error = nil
        requestTask = Task {
            do {
                _ = try await runtime.resourceRequest(operation: "createDirectory", capability: "filesystem.directories.create", parameters: ["path": target])
                try Task.checkCancellation()
                showNewFolder = false; newFolderName = ""; load(target)
            } catch is CancellationError {} catch {
                self.error = error.localizedDescription; busy = false
            }
        }
    }
}
