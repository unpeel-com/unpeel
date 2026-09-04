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

    /// Characters allowed inside a path token. Whitespace, quotes, brackets,
    /// commas and parentheses act as boundaries. `:` is included so a trailing
    /// `:line:col` rides along in the same token (split out later by `parse`).
    private static let pathChars = Set(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._/~-+@:"
    )

    /// Finds the best file-path token in `row` near the clicked `column`.
    /// Prefers a path-shaped token covering the column; if none covers it,
    /// falls back to the nearest path-shaped token (tolerating small column
    /// drift from cell-geometry rounding); if the row has a single path-shaped
    /// token, uses it regardless of column.
    static func match(inRow row: String, column: Int) -> Match? {
        let candidates = tokenize(row).filter { looksLikePath($0.text) }
        guard !candidates.isEmpty else { return nil }

        let chosen = candidates.first { column >= $0.start && column <= $0.end }
            ?? (candidates.count == 1 ? candidates[0] : nearest(candidates, to: column))
        guard let chosen else { return nil }
        return parse(chosen.text)
    }

    private static func tokenize(_ row: String) -> [Token] {
        var tokens: [Token] = []
        var current = ""
        var start = 0
        for (index, char) in row.enumerated() {
            if pathChars.contains(char) {
                if current.isEmpty { start = index }
                current.append(char)
            } else if !current.isEmpty {
                tokens.append(Token(text: current, start: start, end: index - 1))
                current = ""
            }
        }
        if !current.isEmpty {
            tokens.append(Token(text: current, start: start, end: row.count - 1))
        }
        return tokens
    }

    private static func nearest(_ tokens: [Token], to column: Int) -> Token? {
        tokens.min { a, b in distance(a, column) < distance(b, column) }
    }

    private static func distance(_ token: Token, _ column: Int) -> Int {
        if column < token.start { return token.start - column }
        if column > token.end { return column - token.end }
        return 0
    }

    /// A token is a plausible file path if it isn't a URL and either contains a
    /// directory separator or looks like `name.ext`. Avoids matching bare words
    /// and plain numbers.
    private static func looksLikePath(_ token: String) -> Bool {
        guard !token.isEmpty, !token.contains("://") else { return false }
        let base = strippingLineColumn(token).path
        guard base.count >= 2 else { return false }
        if base.contains("/") { return true }
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
    static func absolutePath(_ raw: String, workingDirectory: String?) -> String? {
        var path = raw
        if path.hasPrefix("~") {
            path = (path as NSString).expandingTildeInPath
        }
        if !path.hasPrefix("/") {
            guard let cwd = workingDirectory, !cwd.isEmpty else { return nil }
            path = (cwd as NSString).appendingPathComponent(path)
        }
        path = (path as NSString).standardizingPath
        return path
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
