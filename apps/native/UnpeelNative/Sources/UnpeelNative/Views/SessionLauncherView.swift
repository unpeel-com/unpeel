//
//  SessionLauncherView.swift
//  UnpeelNative
//
//  Main-area "pick a tool" launcher. This is the native port of the Svelte
//  SessionLauncherView the "+" affordance used to open (the sidebar "+" is
//  still a dropdown menu; see SidebarView.newSessionMenu). It is shown in the
//  content area for:
//    - the Finder "New Unpeel Session Here" service (UnpeelStore.openLauncher)
//    - the empty state (no session selected) for the active project
//  Picking a tile launches a session via UnpeelStore.launchSession, which
//  selects the new session and replaces this launcher with the terminal.
//

import SwiftUI

struct SessionLauncherView: View {
    @ObservedObject var store: UnpeelStore
    let project: Project
    /// Compact chrome for an empty pane opened by Split Pane (⌘D). Page is
    /// the full content-area launcher.
    var compact = false
    /// Override the default "launch into this project" action (the pane
    /// launcher binds the new Session without selecting it).
    var onLaunch: ((Preset) -> Void)? = nil
    /// Compact pane launcher only: Escape removes the still-empty pane.
    var onCancel: (() -> Void)? = nil

    @State private var query = ""
    @State private var selectedIndex = 0
    @State private var selectionScrollRequest = 0
    @State private var keyMonitor: Any?
    @FocusState private var searchFocused: Bool

    /// Blank terminal first, then every enabled preset (same content as the
    /// sidebar "+" menu). Display projection, so a scoped workspace/Host
    /// launcher offers THAT Host's presets, not this instance's.
    private var presets: [Preset] {
        [.newTerminal] + store.displayAvailablePresets
    }

    private var filteredPresets: [Preset] {
        let words = query
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(whereSeparator: \.isWhitespace)
            .map { String($0).localizedLowercase }
        guard !words.isEmpty else { return presets }
        return presets.filter { preset in
            let haystack = "\(preset.label) \(preset.command)".localizedLowercase
            return words.allSatisfy { haystack.contains($0) }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: compact ? 8 : 22) {
            if compact {
                searchField
                presetList
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                managePresetsButton
            } else {
                header
                VStack(alignment: .leading, spacing: 10) {
                    presetList
                        .frame(maxWidth: 460)
                        .frame(height: pagePresetListHeight)
                    managePresetsButton
                }
            }
        }
        .padding(compact ? 12 : 40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear {
            guard compact else { return }
            installKeyMonitor()
            // The picker starts clipped at zero width; focus on the next turn
            // so ⌘D immediately routes typing into the mounted search field.
            DispatchQueue.main.async { searchFocused = true }
        }
        .onDisappear { removeKeyMonitor() }
    }

    @ViewBuilder
    private var presetList: some View {
        if filteredPresets.isEmpty {
            Text("No matching presets")
                .font(.system(size: 12))
                .foregroundStyle(Theme.mutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(
                            Array(filteredPresets.enumerated()),
                            id: \.element.id
                        ) { index, preset in
                            LauncherRow(
                                preset: preset,
                                isSelected: compact && index == selectedIndex,
                                action: { launch(preset) },
                                hoverChanged: { hovering in
                                    if compact && hovering { selectedIndex = index }
                                }
                            )
                            .id(preset.id)
                        }
                    }
                }
                // Hover follows the pointer as rows move beneath it during a
                // wheel gesture. Scrolling on every selected-index change
                // therefore creates a feedback loop: hover selects a row,
                // scrollTo recenters it, and another row enters the pointer.
                // Only explicit keyboard/search requests may move the list.
                .onChange(of: selectionScrollRequest) { _ in
                    let index = selectedIndex
                    guard filteredPresets.indices.contains(index) else { return }
                    proxy.scrollTo(filteredPresets[index].id, anchor: .center)
                }
            }
        }
    }

    private var pagePresetListHeight: CGFloat {
        // Keep the page chooser content-sized so it centers as one block;
        // larger preset libraries scroll after thirteen visible rows.
        CGFloat(min(filteredPresets.count, 13)) * 32
    }

    private var managePresetsButton: some View {
        Button {
            store.openSettings(tab: .presets)
        } label: {
            Text("Manage Agents & Apps…")
                .font(.system(size: 11))
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.mutedForeground)
    }

    private var searchField: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
            TextField("Search presets…", text: $query)
                .textFieldStyle(.plain)
                .font(.system(size: 12))
                .focused($searchFocused)
                .onChange(of: query) { _ in
                    selectedIndex = 0
                    selectionScrollRequest &+= 1
                }
                .onSubmit { launchSelected() }
        }
        .padding(.horizontal, 9)
        .frame(height: 32)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Theme.foreground.opacity(0.055))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(Theme.foreground.opacity(0.09), lineWidth: 1)
        )
    }

    private var header: some View {
        VStack(spacing: 6) {
            Text(project.name)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Theme.foreground)
            Text(abbreviatedPath)
                .font(.system(size: 11))
                .foregroundStyle(Theme.foreground.opacity(0.35))
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    private var abbreviatedPath: String {
        SessionTitleDefaults.abbreviatedPath(project.path)
    }

    private func launch(_ preset: Preset) {
        if let onLaunch {
            onLaunch(preset)
        } else {
            store.launchSession(
                projectID: project.id,
                command: preset.command,
                sourcePresetID: preset.command.isEmpty ? nil : preset.id
            )
        }
    }

    // MARK: - Compact picker keyboard

    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            let consumed = MainActor.assumeIsolated { () -> Bool in
                switch event.keyCode {
                case 126: // up
                    moveSelection(-1)
                    return true
                case 125: // down
                    moveSelection(1)
                    return true
                case 36, 76: // return / keypad enter
                    launchSelected()
                    return true
                case 53: // escape
                    onCancel?()
                    return true
                default:
                    return false
                }
            }
            return consumed ? nil : event
        }
    }

    private func removeKeyMonitor() {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
    }

    private func moveSelection(_ delta: Int) {
        let count = filteredPresets.count
        guard count > 0 else { return }
        selectedIndex = (selectedIndex + delta + count) % count
        selectionScrollRequest &+= 1
    }

    private func launchSelected() {
        let choices = filteredPresets
        guard choices.indices.contains(selectedIndex) else { return }
        launch(choices[selectedIndex])
    }
}

/// A single launch choice: tool icon + label on the left and a chevron on the
/// right. Hover and keyboard selection use foreground emphasis without adding
/// a card or row background.
private struct LauncherRow: View {
    let preset: Preset
    var isSelected = false
    let action: () -> Void
    var hoverChanged: (Bool) -> Void = { _ in }

    @State private var hovering = false

    var body: some View {
        let highlighted = hovering || isSelected
        Button(action: action) {
            HStack(spacing: 8) {
                ToolIconView(command: preset.command, size: 16)
                    .opacity(highlighted ? 1 : 0.85)
                Text(preset.label)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(highlighted ? Theme.foreground : Theme.mutedForeground)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 8)
                Image(systemName: "chevron.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.foreground.opacity(highlighted ? 0.5 : 0.25))
            }
            .padding(.horizontal, 8)
            .frame(height: 32)
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover {
            hovering = $0
            hoverChanged($0)
        }
        .help("Start \(preset.label)")
    }
}
