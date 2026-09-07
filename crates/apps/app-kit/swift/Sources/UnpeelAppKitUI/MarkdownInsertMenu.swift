import Foundation

struct MarkdownBackspaceEdit {
    let lineRange: NSRange
    let replacement: String
    let caretUTF16Offset: Int
}

/// Native text-system translation of the Rust Markdown backspace contract.
/// Block/menu vocabulary and replacements remain App-owned and never live in
/// this renderer.
func markdownBackspaceEdit(text: String, selection: NSRange) -> MarkdownBackspaceEdit? {
    guard selection.location != NSNotFound, selection.length == 0 else { return nil }
    let source = text as NSString
    guard selection.location <= source.length else { return nil }
    let lineRangeWithEnding = source.lineRange(
        for: NSRange(location: selection.location, length: 0)
    )
    var contentEnd = NSMaxRange(lineRangeWithEnding)
    while contentEnd > lineRangeWithEnding.location,
        [10, 13].contains(source.character(at: contentEnd - 1))
    {
        contentEnd -= 1
    }
    let lineRange = NSRange(
        location: lineRangeWithEnding.location,
        length: contentEnd - lineRangeWithEnding.location
    )
    let line = source.substring(with: lineRange)
    let indent = String(line.prefix { $0 == " " || $0 == "\t" })
    let rest = String(line.dropFirst(indent.count))
    let markerLength: Int
    if let marker = ["- [ ] ", "- [x] ", "- [X] "].first(where: rest.hasPrefix) {
        markerLength = marker.utf16.count
    } else if let marker = ["- ", "* ", "+ ", "> "].first(where: rest.hasPrefix) {
        markerLength = marker.utf16.count
    } else {
        let hashes = rest.prefix { $0 == "#" }.count
        if (1...6).contains(hashes), rest.dropFirst(hashes).first?.isWhitespace == true {
            markerLength = hashes + 1
        } else {
            let digits = rest.prefix { $0.isNumber }
            let numberedMarker = "\(digits). "
            guard !digits.isEmpty, rest.hasPrefix(numberedMarker) else { return nil }
            markerLength = numberedMarker.utf16.count
        }
    }
    let column = selection.location - lineRange.location
    let prefixLength = indent.utf16.count + markerLength
    guard column > indent.utf16.count, column <= prefixLength else { return nil }
    let body = String(rest.dropFirst(markerLength))
    return MarkdownBackspaceEdit(
        lineRange: lineRange,
        replacement: indent + body,
        caretUTF16Offset: indent.utf16.count
    )
}
