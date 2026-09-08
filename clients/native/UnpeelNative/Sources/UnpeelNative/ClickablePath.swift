//
//  ClickablePath.swift
//  UnpeelNative
//
//  Pulls a file-path token out of a cmd-clicked terminal row. Ghostty only
//  matches URLs/OSC 8 links natively, so bare paths (e.g. `src/Home.tsx:42`
//  printed by an agent) are detected here. Pure string logic — no surface
//  dependency — so it is unit-testable (see ClickablePathTests).
//

import Foundation

/// A retained pane's live OSC 7 directory wins over repeated bootstrap seeds.
/// SwiftUI refreshes must not reset a shell that has changed directory.
struct TerminalWorkingDirectory {
    var seed: String?
    var reported: String?

    var current: String? {
        if let reported, !reported.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return reported
        }
        return seed
    }
}

enum ClickablePath {
    struct Match: Equatable {
        var path: String
        var line: Int?
        var column: Int?
    }

    /// One contiguous run of path characters and the columns it spans.
    private struct Token {
        let text: String
        let start: Int
        let end: Int
    }

    /// Finds the path under a zero-based Character offset in the row. The
    /// terminal wrapper converts grid cells to string offsets before calling.
    /// Never choose a nearby file: that steals clicks on URLs and plain text.
    static func match(inRow row: String, column: Int) -> Match? {
        guard let token = tokenize(row).first(where: {
            column >= $0.start && column <= $0.end
        }), !token.text.contains("://"),
            !token.text.lowercased().hasPrefix("mailto:")
        else { return nil }
        let match = parse(token.text)
        return looksLikePath(match.path) ? match : nil
    }

    private static func tokenize(_ row: String) -> [Token] {
        let characters = Array(row)
        var tokens: [Token] = []
        var index = 0
        let boundaries = "`\"'<>[](),;|"
        while index < characters.count {
            let char = characters[index]
            // Quoted paths can contain spaces, Unicode, brackets and parens.
            if "`\"'".contains(char),
               let end = characters[(index + 1)...].firstIndex(of: char) {
                tokens.append(Token(
                    text: String(characters[(index + 1)..<end]),
                    start: index + 1,
                    end: end - 1
                ))
                index = end + 1
                continue
            }
            if char.isWhitespace || boundaries.contains(char) {
                index += 1
                continue
            }
            let start = index
            var text = ""
            while index < characters.count {
                let next = characters[index]
                if next == "\\", index + 1 < characters.count,
                   characters[index + 1].isWhitespace {
                    text.append(characters[index + 1])
                    index += 2
                    continue
                }
                if next.isWhitespace || boundaries.contains(next) { break }
                text.append(next)
                index += 1
            }
            tokens.append(Token(text: text, start: start, end: index - 1))
        }
        return tokens
    }

    /// A token is a plausible file path if it isn't a URL and either contains a
    /// directory separator or looks like `name.ext`. Avoids matching bare words
    /// and plain numbers.
    private static func looksLikePath(_ token: String) -> Bool {
        guard !token.isEmpty, !token.contains("://") else { return false }
        let base = strippingLineColumn(token).path
        guard base.count >= 2 else { return false }
        if base.contains("/") { return true }
        if base.hasPrefix("."), base.contains(where: { $0.isLetter || $0.isNumber }) { return true }
        // `name.ext` with a short alphanumeric extension.
        guard let dot = base.lastIndex(of: "."), dot != base.startIndex else { return false }
        let ext = base[base.index(after: dot)...]
        return !ext.isEmpty && ext.count <= 8 && ext.allSatisfy { $0.isLetter || $0.isNumber }
    }

    /// Turns a clicked path token into an absolute path to an existing file,
    /// or nil. Absolute and `~` paths are used as-is; relative paths join
    /// `workingDirectory` (the pane's seeded or OSC 7-reported cwd) and are
    /// unresolvable without one. `fileExists` is injectable for tests.
    static func resolveFile(
        _ raw: String,
        workingDirectory: String?,
        fileExists: (String) -> Bool = { path in
            var isDirectory: ObjCBool = false
            return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory)
                && !isDirectory.boolValue
        }
    ) -> String? {
        guard let path = absolutePath(raw, workingDirectory: workingDirectory) else {
            return nil
        }
        return fileExists(path) ? path : nil
    }

    /// Resolve syntax only. Remote Host paths must never be checked against
    /// the Controller's filesystem; their existence is established by the
    /// Host-side command that opens them.
    static func absolutePath(
        _ raw: String,
        workingDirectory: String?,
        homeDirectory: String? = NSHomeDirectory()
    ) -> String? {
        guard !raw.isEmpty else { return nil }
        var path = raw
        if path == "~" || path.hasPrefix("~/") {
            guard let homeDirectory else { return nil }
            path = (homeDirectory as NSString).appendingPathComponent(String(path.dropFirst(2)))
        } else if path.hasPrefix("~") {
            // Resolving another user's home requires Host-owned information.
            return nil
        }
        if !path.hasPrefix("/") {
            guard let cwd = workingDirectory, !cwd.isEmpty else { return nil }
            path = (cwd as NSString).appendingPathComponent(path)
        }
        path = (path as NSString).standardizingPath
        return path
    }

    static func fileURLMatch(_ raw: String, allowRemoteHost: Bool = false) -> Match? {
        guard let url = URL(string: raw), url.isFileURL,
              allowRemoteHost || url.host == nil || url.host == "" || url.host == "localhost",
              url.path.hasPrefix("/"), !url.path.isEmpty
        else { return nil }
        let result = strippingLineColumn(url.path)
        let fragmentLine = url.fragment.flatMap { fragment -> Int? in
            guard fragment.hasPrefix("L"), let line = Int(fragment.dropFirst()), line > 0 else {
                return nil
            }
            return line
        }
        return Match(path: result.path, line: fragmentLine ?? result.line, column: result.column)
    }

    private static func parse(_ token: String) -> Match {
        var trimmed = token
        // Strip trailing punctuation that hugs a path in prose ("see foo.ts.").
        while let last = trimmed.last, ".,:;".contains(last) {
            trimmed.removeLast()
        }
        let result = strippingLineColumn(trimmed)
        return Match(path: result.path, line: result.line, column: result.column)
    }

    /// Splits a trailing `:line` or `:line:col` suffix off a path.
    private static func strippingLineColumn(
        _ token: String
    ) -> (path: String, line: Int?, column: Int?) {
        // Markdown/GitHub-style file references printed by CLI agents.
        if let hash = token.lastIndex(of: "#"),
           let line = Int(token[token.index(after: hash)...].dropFirst()),
           token[token.index(after: hash)...].hasPrefix("L"), line > 0 {
            return (String(token[..<hash]), line, nil)
        }
        let parts = token.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count >= 2 else { return (token, nil, nil) }

        // Only treat the tail as line/col when every trailing part is a number.
        if parts.count >= 3,
           let line = Int(parts[parts.count - 2]),
           let column = Int(parts[parts.count - 1]),
           !parts[parts.count - 3].isEmpty
        {
            let path = parts[0 ..< (parts.count - 2)].joined(separator: ":")
            return (path, line, column)
        }
        if let line = Int(parts[parts.count - 1]),
           !parts[parts.count - 2].isEmpty
        {
            let path = parts[0 ..< (parts.count - 1)].joined(separator: ":")
            return (path, line, nil)
        }
        return (token, nil, nil)
    }
}
