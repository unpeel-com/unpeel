import SwiftUI
import UnpeelAppKitUI

private struct ComponentTreeNode: Identifiable {
    let id: String
    let name: String
    let value: String?
    let children: [ComponentTreeNode]

    static func root(of snapshot: UISnapshot) -> ComponentTreeNode {
        do {
            let data = try JSONEncoder().encode(snapshot.root)
            let object = try JSONSerialization.jsonObject(with: data)
            return make(name: "root", object: object, path: "root")
        } catch {
            return ComponentTreeNode(
                id: "root.error",
                name: "encoding error",
                value: error.localizedDescription,
                children: []
            )
        }
    }

    private static func make(name: String, object: Any, path: String) -> ComponentTreeNode {
        if let dictionary = object as? [String: Any] {
            let type = dictionary["type"] as? String
            let componentID = dictionary["id"] as? String
            let descriptor = [type, componentID.map { "#\($0)" }]
                .compactMap { $0 }
                .joined(separator: " ")
            let priority = [
                "type", "id", "title", "label", "value", "done", "presentation",
                "readOnly", "dirty", "header", "body", "items", "leading", "trailing",
                "accessory", "footer", "bars", "series", "points", "ratio", "accessibilityText",
                "actionRole", "actions", "delete", "activate", "setValue", "submit",
            ]
            let keys = dictionary.keys.sorted { lhs, rhs in
                let left = priority.firstIndex(of: lhs) ?? priority.count
                let right = priority.firstIndex(of: rhs) ?? priority.count
                return left == right ? lhs < rhs : left < right
            }
            return ComponentTreeNode(
                id: path,
                name: name,
                value: descriptor.isEmpty ? nil : descriptor,
                children: keys.map { key in
                    make(name: key, object: dictionary[key]!, path: "\(path).\(key)")
                }
            )
        }
        if let array = object as? [Any] {
            return ComponentTreeNode(
                id: path,
                name: name,
                value: "\(array.count) item\(array.count == 1 ? "" : "s")",
                children: array.enumerated().map { index, item in
                    make(name: "[\(index)]", object: item, path: "\(path)[\(index)]")
                }
            )
        }
        if object is NSNull {
            return ComponentTreeNode(id: path, name: name, value: "null", children: [])
        }
        if let string = object as? String {
            let singleLine = string.replacingOccurrences(of: "\n", with: "↵")
            let clipped = singleLine.count > 90
                ? "\(singleLine.prefix(87))…"
                : singleLine
            return ComponentTreeNode(id: path, name: name, value: "“\(clipped)”", children: [])
        }
        if let number = object as? NSNumber {
            let value = String(cString: number.objCType) == "c"
                ? (number.boolValue ? "true" : "false")
                : number.stringValue
            return ComponentTreeNode(id: path, name: name, value: value, children: [])
        }
        return ComponentTreeNode(
            id: path,
            name: name,
            value: String(describing: object),
            children: []
        )
    }
}

struct ComponentTreeCard: View {
    @ObservedObject var session: HostedAppSession

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 7) {
                Image(systemName: "point.3.connected.trianglepath.dotted")
                Text("COMPONENT TREE")
                    .font(.system(size: 10, weight: .bold, design: .rounded))
                Spacer()
                if let snapshot = session.snapshot {
                    Text("r\(snapshot.revision)")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 38)
            .background(.bar)

            if let snapshot = session.snapshot {
                ScrollView([.vertical, .horizontal]) {
                    ComponentTreeBranch(node: .root(of: snapshot), depth: 0)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .background(Color(nsColor: .textBackgroundColor).opacity(0.35))
                Divider()
                VStack(alignment: .leading, spacing: 3) {
                    Text("\(snapshot.root.component.kind) · #\(snapshot.root.id)")
                    Text("client \(snapshot.clientID) · view \(snapshot.viewID)")
                    Text(session.deliveryLabel)
                }
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
                .padding(9)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.bar)
            } else {
                ContentUnavailableView(
                    "No component tree",
                    systemImage: "point.3.connected.trianglepath.dotted",
                    description: Text(session.connectionLabel)
                )
            }
        }
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(Color.primary.opacity(0.1))
                .frame(width: 1)
        }
    }
}

private struct ComponentTreeBranch: View {
    let node: ComponentTreeNode
    let depth: Int
    @State private var expanded = true

    var body: some View {
        if node.children.isEmpty {
            row
        } else {
            DisclosureGroup(isExpanded: $expanded) {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(node.children) { child in
                        ComponentTreeBranch(node: child, depth: depth + 1)
                    }
                }
                .padding(.leading, 9)
            } label: {
                row
            }
            .disclosureGroupStyle(.automatic)
        }
    }

    private var row: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(node.name)
                .foregroundStyle(node.children.isEmpty ? Color.primary : Color.accentColor)
            if let value = node.value {
                Text(value)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .font(.system(size: 11, design: .monospaced))
        .textSelection(.enabled)
    }
}
