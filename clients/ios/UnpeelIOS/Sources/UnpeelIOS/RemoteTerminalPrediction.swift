import Foundation
import SwiftUI

/// Mosh-style predictive local echo for high-latency transports.
///
/// Over the relay a keystroke's echo pays two WAN traversals, so typing
/// reads as laggy even when the link is healthy. The engine tracks printable
/// keystrokes at the cell the local surface's IME caret reported, renders
/// them immediately as a provisional overlay, and reconciles against the
/// authoritative viewport once server bytes arrive.
///
/// Safety comes from a confidence gate, not from understanding the remote
/// program: predictions are invisible until one of them is CONFIRMED by the
/// real grid (the predicted character appeared at the predicted cell). A
/// context that never echoes recognizably — password prompts, vim normal
/// mode, menus, TUIs that park the caret elsewhere — never earns display,
/// and a contradiction or expiry drops the gate again. Wrong predictions
/// are therefore at worst briefly visible, never destructive: the overlay
/// touches no terminal state.
struct RemoteTerminalPredictionEngine {
    struct Prediction: Equatable {
        var character: Character
        /// 0-based viewport cell where the echo should appear.
        var row: Int
        var column: Int
        var sentAt: Date
    }

    /// A prediction unconfirmed this long means echo is not coming back in
    /// recognizable form; drop everything and close the display gate.
    static let expiry: TimeInterval = 2

    /// Beyond this many unconfirmed keystrokes something is off (key repeat
    /// into a stalled link) — stop predicting rather than paint a phantom
    /// line.
    static let maximumPending = 24

    private(set) var pending: [Prediction] = []
    /// Display gate: earned by the first confirmed prediction, lost on
    /// contradiction or expiry. Tracking continues while the gate is closed
    /// so ordinary echo re-earns it with no user-visible risk.
    private(set) var isConfident = false

    /// Register a printable keystroke. `cursor` is the current caret cell
    /// (used only when nothing is pending — later keystrokes chain off the
    /// previous prediction); nil means the caret is unknown, which makes
    /// prediction impossible.
    mutating func keystroke(
        _ character: Character,
        cursor: (row: Int, column: Int)?,
        columns: Int,
        at now: Date
    ) {
        guard pending.count < Self.maximumPending else {
            clearPending()
            return
        }
        let anchor: (row: Int, column: Int)
        if let last = pending.last {
            anchor = (last.row, last.column + 1)
        } else if let cursor {
            anchor = cursor
        } else {
            clearPending()
            return
        }
        // Wrapping is the remote program's call (soft wrap, composer
        // reflow) — stop predicting at the line edge instead of guessing.
        guard anchor.row >= 0, anchor.column >= 0, anchor.column < columns - 1 else {
            clearPending()
            return
        }
        pending.append(Prediction(
            character: character,
            row: anchor.row,
            column: anchor.column,
            sentAt: now
        ))
    }

    mutating func backspace() {
        guard !pending.isEmpty else { return }
        pending.removeLast()
    }

    /// Anything non-printable (submit, arrows, escape sequences) moves the
    /// cursor in ways only the server knows; keep the earned confidence.
    mutating func clearPending() {
        pending = []
    }

    /// Full reset for replays/rebase/session teardown.
    mutating func reset() {
        pending = []
        isConfident = false
    }

    /// Reconcile against the authoritative viewport after server bytes.
    /// Confirms predictions in order; a foreign character at a predicted
    /// cell is a contradiction and closes the gate; blank cells wait until
    /// `expiry`.
    mutating func reconcile(rows: [Substring], at now: Date) {
        while let first = pending.first {
            if now.timeIntervalSince(first.sentAt) > Self.expiry {
                pending = []
                isConfident = false
                return
            }
            guard first.row < rows.count,
                  let cell = Self.cellCharacter(in: rows[first.row], column: first.column)
            else { return } // beyond current content: still blank, keep waiting
            if cell == first.character {
                pending.removeFirst()
                isConfident = true
                continue
            }
            if cell == " " { return } // echo not painted yet, keep waiting
            // Something else landed where we predicted: wrong context.
            pending = []
            isConfident = false
            return
        }
    }

    /// The provisional characters to draw, only while the gate is open.
    var displayedText: [Character]? {
        guard isConfident, !pending.isEmpty else { return nil }
        return pending.map(\.character)
    }

    var anchor: Prediction? { pending.first }

    /// Character at a display column, assuming one column per Character.
    /// Wide glyphs (CJK, emoji) earlier in the row shift this mapping; the
    /// resulting misread at worst reads as a contradiction, which only
    /// hides the overlay.
    static func cellCharacter(in row: Substring, column: Int) -> Character? {
        guard column >= 0, column < row.count else { return nil }
        return row[row.index(row.startIndex, offsetBy: column)]
    }
}

/// Overlay geometry + content, in unscaled canvas points.
struct RemoteTerminalPredictionOverlayModel: Equatable {
    var characters: [Character]
    var origin: CGPoint
    var cellSize: CGSize
    var fontSize: CGFloat
    var foreground: Color
    var background: Color
}

/// Separate observable so per-keystroke overlay updates invalidate only the
/// small overlay view, never the whole terminal tree (the same trap the
/// keystroke-follow hints hit before becoming a PassthroughSubject).
@MainActor
final class RemoteTerminalPredictionOverlayState: ObservableObject {
    @Published var model: RemoteTerminalPredictionOverlayModel?
}

/// Hands typed bytes (the memory session's host-write path) to the renderer
/// without the session factory needing `self`. Ghostty may deliver on its IO
/// thread; the handler hop to the main actor happens in the renderer.
final class RemoteTerminalPredictionInputTap: @unchecked Sendable {
    private let lock = NSLock()
    private var handler: (@Sendable (Data) -> Void)?

    func setHandler(_ handler: @escaping @Sendable (Data) -> Void) {
        lock.lock()
        self.handler = handler
        lock.unlock()
    }

    func record(_ data: Data) {
        lock.lock()
        let handler = handler
        lock.unlock()
        handler?(data)
    }
}

/// Draws provisional keystrokes cell-aligned over the terminal canvas.
/// Underline marks them as unconfirmed, mosh-style; the cell background
/// masks whatever the stale frame shows underneath.
struct RemoteTerminalPredictionOverlayView: View {
    @ObservedObject var state: RemoteTerminalPredictionOverlayState

    var body: some View {
        if let model = state.model {
            HStack(spacing: 0) {
                ForEach(Array(model.characters.enumerated()), id: \.offset) { _, character in
                    Text(String(character))
                        .font(.system(size: model.fontSize, design: .monospaced))
                        .underline()
                        .foregroundStyle(model.foreground)
                        .frame(
                            width: model.cellSize.width,
                            height: model.cellSize.height
                        )
                        .background(model.background)
                }
            }
            .offset(x: model.origin.x, y: model.origin.y)
            .allowsHitTesting(false)
        }
    }
}
