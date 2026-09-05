import Foundation
import SwiftUI

/// Mosh-style predictive scrolling for remote-rendered TUIs.
///
/// Alternate-screen TUIs (Claude & co) own their scrolling: a flick becomes
/// wheel events, the Mac redraws, frames ride back. Over the relay every
/// visible movement therefore lags a full WAN round trip — the finger moves,
/// the content doesn't. This engine closes that gap by predicting the one
/// thing a wheel almost always means: the content shifts by the rows sent.
/// The view translates the already-rendered canvas in sync with the finger
/// and lets the incoming edge show background until truth arrives.
///
/// Safety mirrors `RemoteTerminalPredictionEngine`: the prediction is a
/// *view-layer translation* that touches no terminal state, it displays only
/// behind a confidence gate earned by an observed wheel→redraw response, and
/// an unanswered gesture eases home and closes the gate — a TUI that
/// ignores wheels is never worse than today, just unimproved. Display is
/// additionally latency-gated per gesture: on a fast path (LAN Wi-Fi) the
/// whole-canvas translation visibly fights a TUI's pinned chrome for no
/// felt benefit, so the engine only translates when measured wheel→pixels
/// time says the user would otherwise be waiting on the network.
///
/// Reconciliation is by OBSERVED CONTENT SHIFT, not per-chunk counting: the
/// renderer measures how many rows the viewport actually moved when a chunk
/// committed and drains exactly that many predicted rows from the front of
/// the queue. A chunk carrying three coalesced redraws drains three batches'
/// worth in the same render pass the content jumps, a streaming chunk that
/// scrolled nothing drains nothing, and the translation always equals
/// "rows sent minus rows already on screen". The earlier 1-chunk-=-1-batch
/// FIFO desynced under coalescing and under unrelated agent output, and its
/// leftover rows sprang back after every flick — the reported bounce.
struct RemoteTerminalScrollPredictionEngine {
    struct PendingBatch: Equatable {
        /// Signed rows: positive = wheel down (content moves up on screen).
        var rows: Int
        var sentAt: Date
        /// Sent while the queue was empty: its send→answered time is pure
        /// path latency. Batches queued behind others measure queue wait
        /// too, which inflated the estimate on fast links whenever the
        /// shift detector missed a chunk — and a few bad samples flipped
        /// the display gate on over Wi-Fi, shoving the canvas mid-scroll.
        var probe: Bool = false
    }

    /// No redraw this long after the oldest unacked send means the TUI is
    /// not answering wheels for this gesture.
    static let responseTimeout: TimeInterval = 0.45

    /// Hard cap on how far prediction may run ahead of truth; beyond this
    /// the placeholder region dominates the viewport.
    static let maximumPendingRows = 20

    /// The translation displays only when wheels take at least this long to
    /// come back as pixels. The canvas translation moves the WHOLE surface —
    /// a TUI's pinned chrome (Claude's composer/footer) included — while the
    /// TUI scrolls only its transcript region in place, so on a fast link
    /// the two visibly fight: the screen slides and reconciles many times a
    /// second for latency nobody was feeling. Below this threshold the
    /// engine still tracks and learns, it just never translates.
    static let displayLatencyThreshold: TimeInterval = 0.18

    private(set) var pending: [PendingBatch] = []
    /// Earned by any observed wheel→shift response, lost when an entire
    /// gesture goes unanswered. Tracking continues while closed so the
    /// next responsive gesture re-earns display with no visible risk.
    private(set) var isConfident = false
    private var ackedThisGesture = false
    /// EWMA of send→answered-on-screen time, sampled at each drain from the
    /// oldest pending batch. Describes the path (LAN vs relay), so it
    /// survives gestures and resets with confidence.
    private(set) var responseLatency: TimeInterval?
    /// Display decision latched at gesture start so the offset can never
    /// pop in or out mid-drag when confidence or the EWMA crosses over.
    private var displaysThisGesture = false

    var pendingRows: Int { pending.reduce(0) { $0 + $1.rows } }

    /// Rows the canvas should translate by right now (negative y per down
    /// row — content follows the finger). Zero until the TUI has proven it
    /// answers wheels AND the path is slow enough for prediction to beat
    /// the real frames.
    var offsetRows: Int { displaysThisGesture ? -pendingRows : 0 }

    mutating func beginGesture() {
        ackedThisGesture = false
        displaysThisGesture = isConfident
            && (responseLatency ?? 0) >= Self.displayLatencyThreshold
    }

    mutating func wheelSent(rows: Int, at now: Date) {
        guard rows != 0 else { return }
        // Stop growing at the cap: the steps still went to the host, so
        // later frames simply drain the tracked portion.
        guard abs(pendingRows + rows) <= Self.maximumPendingRows else { return }
        pending.append(PendingBatch(rows: rows, sentAt: now, probe: pending.isEmpty))
    }

    /// A committed feed moved the viewport content by `rows` (positive =
    /// content moved up, the wheel-down direction). That movement is the TUI
    /// answering the oldest predicted wheels: drain exactly that many rows
    /// from the front of the queue, splitting a partially-answered batch.
    /// Movement opposite the queued direction is not an answer (autoscroll
    /// or a repaint the finger didn't cause) and drains nothing. If the TUI
    /// moved further than predicted, the drain clamps at zero — real frames
    /// already carry the extra motion, so translating past truth would
    /// overshoot. `now` samples the send→on-screen latency that decides
    /// whether future gestures display at all.
    mutating func contentShifted(rows: Int, at now: Date) {
        guard rows != 0, let oldest = pending.first else { return }
        var remaining = rows
        while remaining != 0, let first = pending.first {
            guard (first.rows > 0) == (remaining > 0) else { break }
            if abs(first.rows) <= abs(remaining) {
                remaining -= first.rows
                pending.removeFirst()
            } else {
                pending[0].rows -= remaining
                remaining = 0
            }
        }
        guard remaining != rows else { return }
        if oldest.probe {
            let sample = max(0, now.timeIntervalSince(oldest.sentAt))
            responseLatency = responseLatency.map { $0 * 0.7 + sample * 0.3 } ?? sample
            // A split probe stays queued; it has been sampled and must not
            // report an ever-older age on its next partial answer.
            if pending.first?.sentAt == oldest.sentAt {
                pending[0].probe = false
            }
        }
        ackedThisGesture = true
        isConfident = true
    }

    /// True when the oldest prediction expired unanswered — the caller
    /// eases the translation home. The gate closes only if the whole
    /// gesture produced no ack (coalesced trailing frames must not punish
    /// a TUI that demonstrably responded).
    mutating func expireIfUnanswered(at now: Date) -> Bool {
        guard let oldest = pending.first,
              now.timeIntervalSince(oldest.sentAt) >= Self.responseTimeout
        else {
            return false
        }
        pending.removeAll()
        if !ackedThisGesture {
            isConfident = false
        }
        return true
    }

    /// Gesture cancelled / session detached: drop the translation but keep
    /// earned confidence (it describes the TUI, not the gesture).
    mutating func cancel() {
        pending.removeAll()
    }

    /// Full reset (new session / screen replaced): confidence and the
    /// path-latency estimate must be re-earned against whatever now owns
    /// the terminal.
    mutating func resetConfidence() {
        pending.removeAll()
        isConfident = false
        ackedThisGesture = false
        responseLatency = nil
        displaysThisGesture = false
    }
}

/// Measures how many rows the rendered viewport moved between two reads —
/// the reconciliation signal for `contentShifted(rows:)`. Pure text
/// alignment over the two viewport snapshots: for each candidate shift the
/// score is the number of identical non-blank rows, and the smallest shift
/// wins ties, so a stationary screen (or one repainted beyond recognition)
/// reads as zero rather than guessing. Positive = content moved up on
/// screen (the wheel-down direction), matching the engine's convention.
///
/// TUIs with fixed chrome (Claude pins its composer and footer while the
/// transcript scrolls) still resolve correctly: the scrolled region
/// outnumbers the pinned rows, so the true shift outscores zero. When it
/// doesn't, under-reporting is safe — the leftover translation drains via
/// the expiry ease-home instead of a wrong jump.
enum RemoteTerminalScrollShiftDetector {
    /// Fewer than this many agreeing non-blank rows means the screen
    /// changed too much to trust any alignment, including zero.
    static let minimumMatches = 2

    static func shift(before: String, after: String, maxShift: Int) -> Int {
        guard maxShift > 0 else { return 0 }
        let beforeRows = rows(of: before)
        let afterRows = rows(of: after)
        let count = min(beforeRows.count, afterRows.count)
        guard count > 0 else { return 0 }

        var bestShift = 0
        var bestScore = -1
        for magnitude in 0...maxShift {
            for candidate in magnitude == 0 ? [0] : [magnitude, -magnitude] {
                var score = 0
                for index in 0..<count {
                    let source = index + candidate
                    guard source >= 0, source < beforeRows.count else { continue }
                    let row = afterRows[index]
                    guard !row.isEmpty, row == beforeRows[source] else { continue }
                    score += 1
                }
                // Strictly greater: |candidate| grows through the loop, so
                // ties resolve to the smallest movement.
                if score > bestScore {
                    bestScore = score
                    bestShift = candidate
                }
            }
        }
        guard bestScore >= Self.minimumMatches else { return 0 }
        return bestShift
    }

    /// Viewport text split into rows with trailing spaces dropped, so
    /// ghostty's cell padding never breaks an otherwise identical row.
    private static func rows(of text: String) -> [Substring] {
        text.split(separator: "\n", omittingEmptySubsequences: false).map { line in
            var line = line
            while line.last == " " { line = line.dropLast() }
            return line
        }
    }
}

/// Published translation consumed by the canvas modifier, isolated from the
/// renderer's other state so offset changes re-evaluate only the modifier.
final class RemoteTerminalScrollPredictionState: ObservableObject {
    @Published var offsetY: CGFloat = 0
}

/// Applies the predicted translation in canvas coordinates (inside the
/// zoom/pan transform, so predicted rows scale with the grid). Immediate
/// while tracking and while frames absorb it (content and translation move
/// in the same render pass); the renderer publishes expiry ease-home inside
/// an animation block.
struct RemoteTerminalScrollPredictionOffsetModifier: ViewModifier {
    @ObservedObject var state: RemoteTerminalScrollPredictionState

    func body(content: Content) -> some View {
        content.offset(y: state.offsetY)
    }
}
