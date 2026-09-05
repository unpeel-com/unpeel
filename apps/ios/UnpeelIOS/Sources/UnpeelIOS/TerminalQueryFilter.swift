import Foundation

/// Strips terminal *query request* sequences from remote output before it is
/// fed to the phone's local ghostty surface.
///
/// The phone renders through its own local ghostty surface, but that surface
/// must never answer terminal queries embedded in the remote output: the
/// Mac's real terminal already answered them, and the phone surface's reply is
/// routed upstream as spurious *input* (e.g. Grok's XTVERSION query yields
/// `>|ghostty 1.3.1` typed into its prompt, re-emitted on every focus-driven
/// replay). Queries carry no display content, so removing them is invisible.
///
/// The filter is **stateful across chunks** (one instance per renderer, like
/// `RemoteTerminalMouseModeTracker`): a query split across a chunk boundary
/// would otherwise pass through in two innocent-looking halves, reassemble
/// inside the surface's parser, and get answered — the `[?2026l`-shaped
/// composer leak. An incomplete trailing sequence that could still become a
/// query is withheld and prepended to the next chunk; this is
/// display-equivalent, since the surface's own parser would buffer the same
/// bytes without rendering anything. A withheld run that outgrows any real
/// query is dropped rather than emitted (same rationale as the mouse tracker's
/// pending cap); an unterminated DCS query flips a discard flag instead of
/// buffering, so its payload never accumulates.
///
/// Stripped: CSI DA (`…c`), DSR (`…n`), XTVERSION (`CSI > … q`), DECRQM
/// (`CSI … $ p`), and DCS XTGETTCAP/DECRQSS requests (`ESC P +q…` / `$q…`).
/// `ESC c` (RIS) and DECSCUSR (`CSI … SP q`) are deliberately preserved.
final class TerminalQueryFilter: @unchecked Sendable {
    /// A withheld (unterminated) CSI prefix longer than this is not a real
    /// query — drop it entirely rather than emitting a reassembly hazard.
    private static let maximumCarryBytes = 96

    private let lock = NSLock()
    private var carry = [UInt8]()
    /// Inside an unterminated DCS query: discard bytes until ST/BEL.
    private var discardingDCSQuery = false

    /// Clears carried state. Call wherever the byte stream restarts from
    /// scratch (reset/clear replays), alongside the mouse-tracker reset.
    func reset() {
        lock.lock()
        defer { lock.unlock() }
        carry.removeAll(keepingCapacity: true)
        discardingDCSQuery = false
    }

    func stripRequests(_ input: Data) -> Data {
        lock.lock()
        defer { lock.unlock() }
        if carry.isEmpty, !discardingDCSQuery, !input.contains(0x1B) { return input }
        var bytes = carry
        carry = []
        bytes.append(contentsOf: input)
        let n = bytes.count
        var out = [UInt8]()
        out.reserveCapacity(n)
        var i = 0
        if discardingDCSQuery {
            switch scanDCSTerminator(bytes, from: 0) {
            case .terminated(let end):
                discardingDCSQuery = false
                i = end
            case .trailingEsc:
                carry = [0x1B] // split ST — hold the ESC for the next chunk
                return Data()
            case .exhausted:
                return Data() // whole chunk is query payload
            }
        }
        while i < n {
            let b = bytes[i]
            if b != 0x1B { out.append(b); i += 1; continue }
            guard i + 1 < n else {
                carry = [0x1B] // lone trailing ESC — may begin a query
                break
            }
            let next = bytes[i + 1]
            if next == 0x5B { // CSI: ESC [
                var j = i + 2
                var priv: UInt8?
                if j < n, (0x3C...0x3F).contains(bytes[j]) { priv = bytes[j]; j += 1 }
                var hasDollar = false
                while j < n, !(0x40...0x7E).contains(bytes[j]) {
                    if bytes[j] == 0x24 { hasDollar = true }
                    j += 1
                }
                guard j < n else {
                    // No final byte yet: withhold so a split query cannot
                    // reassemble in the surface. Oversized ⇒ not a query; drop.
                    if n - i <= Self.maximumCarryBytes {
                        carry = Array(bytes[i..<n])
                    }
                    break
                }
                let final = bytes[j]
                let strip: Bool
                switch final {
                case 0x63: strip = true             // 'c' — Device Attributes
                case 0x6E: strip = true             // 'n' — Device Status Report
                case 0x71: strip = priv == 0x3E     // '>…q' — XTVERSION (not DECSCUSR)
                case 0x70: strip = hasDollar        // '…$p' — DECRQM (not '!p' DECSTR)
                default: strip = false
                }
                if strip { i = j + 1 } else { out.append(contentsOf: bytes[i...j]); i = j + 1 }
            } else if next == 0x50 { // DCS: ESC P
                guard i + 3 < n else {
                    // Too short to classify as '+q'/'$q' — withhold the tail.
                    carry = Array(bytes[i..<n])
                    break
                }
                let d0 = bytes[i + 2], d1 = bytes[i + 3]
                let isQuery = (d0 == 0x2B || d0 == 0x24) && d1 == 0x71 // '+q' / '$q'
                switch scanDCSTerminator(bytes, from: i + 2) {
                case .terminated(let end):
                    if isQuery { i = end } else { out.append(contentsOf: bytes[i..<end]); i = end }
                case .trailingEsc:
                    if isQuery {
                        discardingDCSQuery = true
                        carry = [0x1B] // split ST — hold the ESC for the next chunk
                    } else {
                        out.append(contentsOf: bytes[i..<n])
                    }
                    i = n
                case .exhausted:
                    if isQuery {
                        discardingDCSQuery = true // swallow payload chunk-by-chunk
                    } else {
                        out.append(contentsOf: bytes[i..<n])
                    }
                    i = n
                }
            } else {
                out.append(b); i += 1 // ESC + other (e.g. RIS 'ESC c') — preserve
            }
        }
        return Data(out)
    }

    private enum DCSScan {
        case terminated(end: Int) // index just past BEL / ESC \
        case trailingEsc          // chunk ends with a lone ESC (maybe split ST)
        case exhausted            // no terminator in this chunk
    }

    private func scanDCSTerminator(_ bytes: [UInt8], from start: Int) -> DCSScan {
        var j = start
        let n = bytes.count
        while j < n {
            if bytes[j] == 0x07 { return .terminated(end: j + 1) } // BEL
            if bytes[j] == 0x1B {
                guard j + 1 < n else { return .trailingEsc }
                if bytes[j + 1] == 0x5C { return .terminated(end: j + 2) } // ESC \
            }
            j += 1
        }
        return .exhausted
    }
}
