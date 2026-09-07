import Foundation
import UnpeelAppKitUI

extension DemoKind {
    var expectedSemanticScreens: [String] {
        switch self {
        case .usageApp:
            ["Provider catalog", "Provider detail", "Alerts"]
        case .diffsApp:
            ["Changed-file list", "Full diff detail"]
        case .githubIssuesApp:
            ["Filtered issue list", "Full issue detail"]
        case .markdownApp:
            [
                "Workspace chooser", "Note picker", "New-note form", "Markdown editor",
                "Editor context menu", "Slash insert menu",
            ]
        case .filetreeApp:
            ["Root tree", "Nested tree"]
        case .charts, .todo, .markdown, .media, .surface, .canvas:
            []
        }
    }
}

@MainActor
extension HostedAppSession {
    var walkthroughComplete: Bool {
        let observed = Set(observedScreens)
        return !kind.expectedSemanticScreens.isEmpty
            && kind.expectedSemanticScreens.allSatisfy(observed.contains)
    }

    func walkEveryScreen() {
        guard kind.isCrossPlatformAuditApp else { return }
        observedScreens = []
        walkthroughStep = 0
        walkthroughStatus = "Walking…"
        if let snapshot { observeWalkthrough(snapshot) }
    }

    func observeWalkthrough(_ snapshot: UISnapshot) {
        for screen in semanticScreens(in: snapshot) where !observedScreens.contains(screen) {
            observedScreens.append(screen)
        }
        if walkthroughComplete {
            walkthroughStep = nil
            walkthroughStatus =
                "Passed · \(observedScreens.count)/\(kind.expectedSemanticScreens.count)"
            return
        }
        guard let step = walkthroughStep,
            let (nextStep, action) = walkthroughAction(step: step, snapshot: snapshot)
        else { return }
        walkthroughStep = nextStep
        sendPrimary(action)
    }

    static func coverageReport(for sessions: [HostedAppSession]) -> String {
        var lines = [
            "# Kitchen Sink cross-platform semantic audit",
            "",
            "Each entry was spawned in a real libghostty PTY and driven through the same "
                + "Unix-socket semantic session consumed by SwiftUI and the web renderer.",
            "",
        ]
        for session in sessions {
            let observed = Set(session.observedScreens)
            let missing = session.kind.expectedSemanticScreens.filter { !observed.contains($0) }
            lines.append("## \(session.title)")
            lines.append("")
            lines.append("- Semantic screens: \(session.observedScreens.joined(separator: ", "))")
            lines.append(
                "- Terminal-only surfaces: "
                    + (missing.isEmpty ? "none" : missing.joined(separator: ", "))
            )
            lines.append("- Result: \(missing.isEmpty ? "PASS" : "INCOMPLETE")")
            lines.append("- Walker: \(session.walkthroughStatus)")
            lines.append("- Walker step: \(session.walkthroughStep.map(String.init) ?? "complete")")
            lines.append("- Process: \(session.processState.label)")
            lines.append("- Connection: \(session.connectionLabel)")
            if let fallback = session.fallbackMessage {
                lines.append("- Renderer diagnostic: \(fallback)")
            }
            if let ack = session.lastAck {
                lines.append(
                    "- Last ack: \(ack.status.rawValue) at r\(ack.revision)"
                        + (ack.message.map { " · \($0)" } ?? "")
                )
            }
            lines.append("")
        }
        return lines.joined(separator: "\n")
    }

    private func semanticScreens(in snapshot: UISnapshot) -> [String] {
        switch (kind, snapshot.root.component) {
        case (.usageApp, .page(let page)):
            if page.title == "Usage" { return ["Provider catalog"] }
            if page.title == "Alerts" { return ["Alerts"] }
            return ["Provider detail"]
        case (.diffsApp, .page(let page)):
            if case .content = page.body { return ["Full diff detail"] }
            return ["Changed-file list"]
        case (.githubIssuesApp, .page(let page)):
            if case .content = page.body { return ["Full issue detail"] }
            return ["Filtered issue list"]
        case (.markdownApp, .page(let page)):
            if page.title == "Choose your notes folder" { return ["Workspace chooser"] }
            if page.title == "New note" { return ["New-note form"] }
            return []
        case (.markdownApp, .tree):
            return ["Note picker"]
        case (.markdownApp, .markdownEditor(let editor)):
            var screens = ["Markdown editor"]
            if editor.contextMenu != nil { screens.append("Editor context menu") }
            if editor.insertMenu != nil { screens.append("Slash insert menu") }
            return screens
        case (.filetreeApp, .tree(let tree)):
            let location = tree.location.components(separatedBy: " · ").first ?? tree.location
            return [location == "." ? "Root tree" : "Nested tree"]
        default:
            return []
        }
    }

    private func walkthroughAction(
        step: Int,
        snapshot: UISnapshot
    ) -> (Int, UIAction)? {
        switch kind {
        case .usageApp:
            return usageAction(step: step, snapshot: snapshot)
        case .diffsApp:
            return listDetailAction(
                step: step,
                snapshot: snapshot,
                excluding: ["refresh-diffs"]
            )
        case .githubIssuesApp:
            return listDetailAction(
                step: step,
                snapshot: snapshot,
                excluding: ["refresh-issues", "issue-status"]
            )
        case .markdownApp:
            return markdownAction(step: step, snapshot: snapshot)
        case .filetreeApp:
            return fileTreeAction(step: step, snapshot: snapshot)
        case .charts, .todo, .markdown, .media, .surface, .canvas:
            return nil
        }
    }

    private func usageAction(step: Int, snapshot: UISnapshot) -> (Int, UIAction)? {
        guard case .page(let page) = snapshot.root.component else { return nil }
        switch step {
        case 0:
            guard page.title == "Usage", case .list(let list) = page.body,
                let item = list.items.first(where: {
                    $0.activate != nil && $0.label != "Refresh" && $0.label != "Alerts"
                }), let activate = item.activate
            else { return nil }
            return (1, UIAction(nodeID: item.id, action: activate, kind: .activate))
        case 1:
            guard page.title != "Usage", page.title != "Alerts", let back = page.back else {
                return nil
            }
            return (2, UIAction(nodeID: snapshot.root.id, action: back, kind: .cancel))
        case 2:
            guard page.title == "Usage",
                let alerts = page.footer.actions.first(where: { $0.id == "open-alerts" })
            else { return nil }
            return (
                3,
                UIAction(nodeID: alerts.id, action: alerts.action, kind: .activate)
            )
        default:
            return nil
        }
    }

    private func listDetailAction(
        step: Int,
        snapshot: UISnapshot,
        excluding ids: [String]
    ) -> (Int, UIAction)? {
        guard step == 0, case .page(let page) = snapshot.root.component,
            case .list(let list) = page.body,
            let item = list.items.first(where: {
                !ids.contains($0.id) && $0.activate != nil
            }), let activate = item.activate
        else { return nil }
        return (1, UIAction(nodeID: item.id, action: activate, kind: .activate))
    }

    private func markdownAction(step: Int, snapshot: UISnapshot) -> (Int, UIAction)? {
        switch (step, snapshot.root.component) {
        case (0, .page(let page)) where page.title == "Choose your notes folder":
            guard case .input(let input)? = page.header, let submit = input.submit else {
                return nil
            }
            let vault = URL(fileURLWithPath: sessionDirectory, isDirectory: true)
                .appendingPathComponent("workspace/vault", isDirectory: true).path
            return (
                1,
                UIAction(
                    nodeID: input.id,
                    action: submit,
                    kind: .submit,
                    value: .text(vault)
                )
            )
        case (1, .tree(let tree)):
            guard let create = tree.footer.actions.first(where: { $0.id == "new-note" }) else {
                return nil
            }
            return (
                2,
                UIAction(nodeID: create.id, action: create.action, kind: .activate)
            )
        case (2, .page(let page)) where page.title == "New note":
            guard let back = page.back else { return nil }
            return (3, UIAction(nodeID: snapshot.root.id, action: back, kind: .cancel))
        case (3, .tree(let tree)):
            guard let file = tree.items.first(where: { $0.kind == .file }) else { return nil }
            return (
                4,
                UIAction(
                    nodeID: snapshot.root.id,
                    action: tree.actions.open,
                    kind: .activate,
                    value: .text(file.id)
                )
            )
        case (4, .markdownEditor(let editor)):
            guard editor.insertMenu == nil, let replace = editor.actions.replaceRange else {
                return nil
            }
            let lines = editor.text.split(separator: "\n", omittingEmptySubsequences: false)
            let end = UITextPosition(
                line: max(0, lines.count - 1),
                utf16Column: lines.last.map { String($0).utf16.count } ?? 0
            )
            return (
                5,
                UIAction(
                    nodeID: snapshot.root.id,
                    action: replace,
                    kind: .change,
                    value: .textEdit(
                        UITextEdit(
                            range: UITextRange(start: end, end: end),
                            text: "\n/"
                        ))
                )
            )
        default:
            return nil
        }
    }

    private func fileTreeAction(step: Int, snapshot: UISnapshot) -> (Int, UIAction)? {
        guard step == 0, case .tree(let tree) = snapshot.root.component,
            let directory = tree.items.first(where: { $0.kind == .directory })
        else { return nil }
        return (
            1,
            UIAction(
                nodeID: snapshot.root.id,
                action: tree.actions.open,
                kind: .activate,
                value: .text(directory.id)
            )
        )
    }
}
