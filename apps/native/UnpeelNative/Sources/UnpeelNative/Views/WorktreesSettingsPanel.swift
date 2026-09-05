//
//  WorktreesSettingsPanel.swift
//  UnpeelNative
//
//  Settings ▸ Worktrees: gated on the Git worktrees experimental feature.
//  Home of the "Show agent worktrees" toggle (provider-created checkouts
//  in the sidebar) and a list of this workspace's worktree child projects
//  with create / reveal / remove.
//

import SwiftUI

struct WorktreesSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    private var isLocalScope: Bool { store.selectedHostScope == .local }
    private var isLocalMachine: Bool { store.selectedHostScope.isLocalMachine }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Worktrees",
                        description: "Isolated git checkouts so several agents can "
                            + "work the same repo without touching each other's files. "
                            + "Create one from a git project, or show the worktrees "
                            + "agents make for themselves."
                    )
                    .padding(.bottom, 4)
                }

                if isLocalScope {
                    agentWorktreesSection
                    if store.isExperimentalEnabled(.sessionsMcp) {
                        sessionCreateSection
                    }
                }
                worktreesListSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }

    /// Provider-created linked worktrees in the sidebar. Local-scope only:
    /// discovery runs in this instance's UnpeelStore, not on a remote Host.
    private var agentWorktreesSection: some View {
        Section {
            LabeledContent {
                Toggle("", isOn: $store.showAgentWorktrees)
                    .toggleStyle(.switch)
                    .labelsHidden()
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Show agent worktrees")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text("Show the working copies agents create for themselves "
                        + "(Claude Code and similar) as folders under their "
                        + "project in the sidebar.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Agent worktrees",
                description: "Some agents make their own git worktree of a "
                    + "project to work in. Turn this on to see those worktrees "
                    + "in the sidebar; turning it off hides them again without "
                    + "deleting anything."
            )
        }
    }

    /// Same MCP grant as Settings ▸ Sessions use, surfaced here because it
    /// is a worktree permission. Bound to the same store field.
    private var sessionCreateSection: some View {
        Section {
            LabeledContent {
                Toggle(
                    "",
                    isOn: Binding(
                        get: { store.mcpWorktreeAccess },
                        set: { store.setMcpWorktreeAccess($0) }
                    )
                )
                .toggleStyle(.switch)
                .labelsHidden()
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Let sessions create worktrees")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text("Agents can prepare isolated git worktrees as child "
                        + "projects in the sidebar. Launching sessions into "
                        + "them is still up to you. Applies immediately.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Session access",
                description: "Whether sessions may prepare worktrees through "
                    + "Unpeel MCP. They still cannot launch sessions themselves."
            )
        }
    }

    private var worktreesListSection: some View {
        Section {
            if worktreeRows.isEmpty {
                Text(emptyListCopy)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(worktreeRows) { row in
                    worktreeRow(row)
                }
            }

            if isLocalMachine, !gitParentProjects.isEmpty {
                newWorktreeControl
            }
        } header: {
            SettingsSectionHeader(
                title: "This workspace",
                description: worktreeRows.isEmpty
                    ? "Worktrees registered under this workspace's projects."
                    : "Each worktree is a child folder of its project in the sidebar."
            )
        }
    }

    private var emptyListCopy: String {
        if isLocalScope, !store.showAgentWorktrees {
            return "No worktrees in this workspace. Create one from a git "
                + "project, or turn on Show agent worktrees to adopt copies "
                + "agents make for themselves."
        }
        return "No worktrees in this workspace."
    }

    @ViewBuilder
    private var newWorktreeControl: some View {
        let parents = gitParentProjects
        if parents.count == 1, let parent = parents.first {
            Button {
                store.promptCreateWorktree(projectID: parent.id)
            } label: {
                Label("New worktree…", systemImage: "plus")
            }
        } else {
            Menu {
                ForEach(parents) { parent in
                    Button(parent.name) {
                        store.promptCreateWorktree(projectID: parent.id)
                    }
                }
            } label: {
                Label("New worktree…", systemImage: "plus")
            }
        }
    }

    private func worktreeRow(_ row: WorktreeRow) -> some View {
        HStack(spacing: 10) {
            ChromeIconView(icon: .branch, size: 13)
                .foregroundStyle(Theme.mutedForeground)
                .frame(width: 18)

            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(row.project.name)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                        .lineLimit(1)
                    if row.isAgentCreated {
                        Text("Agent")
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(Theme.mutedForeground)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Theme.mutedForeground.opacity(0.14), in: Capsule())
                    }
                }
                Text(row.detail)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(row.project.path)
            }

            Spacer(minLength: 8)

            if row.sessionCount > 0 {
                Text(row.sessionCount == 1 ? "1 session" : "\(row.sessionCount) sessions")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
            }

            if isLocalMachine || isLocalScope {
                Menu {
                    if isLocalMachine {
                        Button("Reveal in Finder") {
                            store.revealInFinder(path: row.project.path)
                        }
                    }
                    if isLocalScope {
                        if isLocalMachine { Divider() }
                        Button("Remove worktree", role: .destructive) {
                            store.removeWorktreeProject(row.project.id)
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.mutedForeground)
                        .frame(width: 22, height: 22)
                        .contentShape(Rectangle())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help("More…")
            }
        }
        .padding(.vertical, 2)
        .contextMenu {
            if isLocalMachine {
                Button("Reveal in Finder") {
                    store.revealInFinder(path: row.project.path)
                }
            }
            if isLocalScope {
                Button("Remove worktree", role: .destructive) {
                    store.removeWorktreeProject(row.project.id)
                }
            }
        }
    }

    // MARK: - Data

    private struct WorktreeRow: Identifiable {
        let project: Project
        let parentName: String
        let sessionCount: Int

        var id: String { project.id }

        var isAgentCreated: Bool {
            project.id.hasPrefix("native-auto-worktree-")
        }

        var detail: String {
            let branch = project.worktreeBranch ?? ""
            if parentName.isEmpty {
                return branch.isEmpty ? project.path : "\(branch) · \(project.path)"
            }
            if branch.isEmpty {
                return "\(parentName) · \(project.path)"
            }
            return "\(parentName) · \(branch) · \(project.path)"
        }
    }

    private var worktreeRows: [WorktreeRow] {
        let projects = store.displayProjectsByID
        return projects.values
            .filter(\.isWorktree)
            .map { project in
                let parentName = project.parentProjectID
                    .flatMap { projects[$0]?.name } ?? "Project"
                let sessionCount = store.findDisplayNode(project.id)?.sessions.count ?? 0
                return WorktreeRow(
                    project: project,
                    parentName: parentName,
                    sessionCount: sessionCount
                )
            }
            .sorted {
                if $0.parentName != $1.parentName {
                    return $0.parentName.localizedCaseInsensitiveCompare($1.parentName)
                        == .orderedAscending
                }
                return $0.project.name.localizedCaseInsensitiveCompare($1.project.name)
                    == .orderedAscending
            }
    }

    /// Top-level git projects that can grow a new worktree. Matches the
    /// sidebar's "New worktree…" gate (real project, not a worktree child).
    private var gitParentProjects: [Project] {
        store.displayProjectsByID.values
            .filter {
                $0.parentProjectID == nil
                    && $0.isFolder != true
                    && $0.worktreeBranch == nil
                    && UnpeelStore.isGitRepo(path: $0.path)
            }
            .sorted {
                $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending
            }
    }
}
