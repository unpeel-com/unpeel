import Foundation

/// What to do with an incoming terminal-output frame given the byte offset
/// the client has already consumed (`held`). Extracted as a pure decision so
/// the streaming resync logic is deterministically testable — the source of
/// the "disconnected, can't reconnect" loop was the WS handler treating
/// *every* offset mismatch as a full teardown+reconnect.
enum StreamFrameAction: Equatable {
    /// Frame continues exactly where we are — feed the whole payload.
    case feed
    /// Frame starts before `held` but extends past it (overlap on resume):
    /// feed only the new tail, dropping the first `dropLeading` bytes.
    case feedSuffix(dropLeading: Int)
    /// Frame is entirely bytes we already have (a stale/duplicate frame after
    /// a reconnect): drop it, no restart.
    case skip
    /// Frame is *ahead* of us — we fell behind the live broadcaster and
    /// missed `[from, upTo)`. Fetch that gap out-of-band, feed it, then feed
    /// this frame — instead of tearing the whole connection down.
    case fillGap(from: UInt64, upTo: UInt64)
}

enum StreamFrameReconciler {
    /// Decide how to reconcile a frame at `frameOffset` (length `frameLength`)
    /// against the consumed offset `held`. Never returns "restart": a forward
    /// gap is filled, a stale frame is skipped, an overlap is trimmed — the
    /// live connection stays up.
    static func action(held: UInt64, frameOffset: UInt64, frameLength: Int) -> StreamFrameAction {
        if frameOffset == held { return .feed }

        if frameOffset > held {
            // We're behind: bytes [held, frameOffset) were skipped by the
            // live stream. Fill them before feeding this frame.
            return .fillGap(from: held, upTo: frameOffset)
        }

        // frameOffset < held — the frame starts in bytes we already have.
        let frameEnd = frameOffset + UInt64(max(0, frameLength))
        if frameEnd <= held {
            // Entirely old — a duplicate/replayed frame. Drop it.
            return .skip
        }
        // Straddles `held`: keep only the part we haven't seen.
        return .feedSuffix(dropLeading: Int(held - frameOffset))
    }
}
