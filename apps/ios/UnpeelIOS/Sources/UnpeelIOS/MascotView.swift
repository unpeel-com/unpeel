import SwiftUI
import UIKit

/// The Unpeel pixel mascot — the little creature idling in the website
/// footer (`apps/website/public/mascot-animated.webp`) — so the connection
/// screens have a friendly face to wait with instead of a bare SF Symbol.
///
/// Renders the REAL animation: the webp's four 13×13-pixel frames are
/// bundled in the asset catalog (`MascotFrame0…3`, extracted losslessly from
/// `unpeel-mascot/mascot-animated.webp` at native pixel resolution) and
/// stepped on the source's chunky 200ms cadence. `interpolation(.none)`
/// keeps the pixels crisp at any rendered size. Decorative only — hidden
/// from accessibility, and Reduce Motion freezes it on the first (resting)
/// frame. If the bundled frames ever fail to load, the hand-drawn Canvas
/// recreation below (`MascotFrame`) takes over as a fallback.
struct PixelMascotView: View {
    /// Rendered width in points (the frames are square).
    var size: CGFloat = 72

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// The webp's frame cadence: 4 frames × 200ms.
    private static let frameDuration: TimeInterval = 0.2

    /// The bundled animation frames, in playback order — or nil if the asset
    /// catalog is missing any of them (then the Canvas fallback draws).
    private static let bundledFrames: [UIImage]? = {
        let frames = (0..<4).compactMap { UIImage(named: "MascotFrame\($0)") }
        return frames.count == 4 ? frames : nil
    }()

    var body: some View {
        Group {
            if let frames = Self.bundledFrames {
                if reduceMotion {
                    frameImage(frames[0])
                } else {
                    // Periodic (not `.animation`) so the idle loop redraws at
                    // 5 Hz instead of display rate — the motion is
                    // intentionally stepped, exactly the webp's frames.
                    // Indexing off wall time keeps every mascot on screen in
                    // sync.
                    TimelineView(.periodic(from: .now, by: Self.frameDuration)) { context in
                        let tick = Int(
                            context.date.timeIntervalSinceReferenceDate / Self.frameDuration
                        )
                        frameImage(frames[tick % frames.count])
                    }
                }
            } else if reduceMotion {
                MascotFrame(pose: .resting)
            } else {
                TimelineView(.periodic(from: .now, by: 0.1)) { context in
                    MascotFrame(pose: .at(context.date))
                }
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    private func frameImage(_ image: UIImage) -> some View {
        Image(uiImage: image)
            .interpolation(.none)
            .resizable()
            .scaledToFit()
    }
}

/// FALLBACK ONLY: a hand-drawn Canvas recreation of one mascot frame, used
/// when the bundled `MascotFrame0…3` assets fail to load (see
/// `PixelMascotView.bundledFrames`). Same 13×13 grid and canonical rainbow
/// palette as the source art in `unpeel-mascot/mascot-logo.svg` (the
/// agent-accent sweep: teal → green → blue → purple, with the lightened face
/// band and black eyes). Its idle loop approximates the webp's feel: the
/// tail flicks on the same 200ms cadence, plus an occasional blink and a
/// one-pixel bob. Split out so the Reduce Motion path can render a single
/// static frame with no timeline behind it.
private struct MascotFrame: View {
    struct Pose: Equatable {
        var eyesClosed = false
        var tailFlicked = false
        var bobbed = false

        static let resting = Pose()

        /// The idle loop, derived from wall time so every mascot on screen
        /// breathes in sync: a constant 0.8s tail wag (the webp's 4×200ms
        /// cycle), a blink beat every ~3.7s, and a slow one-pixel bob.
        static func at(_ date: Date) -> Pose {
            let t = date.timeIntervalSinceReferenceDate
            return Pose(
                eyesClosed: t.truncatingRemainder(dividingBy: 3.7) < 0.2,
                tailFlicked: t.truncatingRemainder(dividingBy: 0.8) < 0.4,
                bobbed: t.truncatingRemainder(dividingBy: 2.4) < 1.2
            )
        }
    }

    let pose: Pose

    var body: some View {
        Canvas { context, canvasSize in
            let cell = min(canvasSize.width, canvasSize.height) / CGFloat(Self.gridSide)
            func fill(_ x: Int, _ y: Int, _ hex: UInt32) {
                // Slight overlap so adjacent cells can't show hairline seams
                // at non-integral display scales.
                let rect = CGRect(
                    x: CGFloat(x) * cell, y: CGFloat(y) * cell,
                    width: cell + 0.4, height: cell + 0.4
                )
                context.fill(Path(rect), with: .color(Color(hex: hex)))
            }

            for (y, row) in Self.grid.enumerated() {
                for (x, token) in row.enumerated() where token != "." {
                    fill(x, y, Self.palette[token] ?? 0x4FA8FF)
                }
            }

            for (x, y) in Self.tailBase { fill(x, y, Self.tailHex) }
            for (x, y) in pose.tailFlicked ? Self.tailTipFlicked : Self.tailTipUp {
                fill(x, y, Self.tailHex)
            }

            // Eyes are drawn over the face band so a blink can reveal the
            // face tint beneath: open = the full 1×2-cell eye, closed = a
            // sliver of lid at the bottom of the lower cell.
            for eyeX in [3, 7] {
                let rect = pose.eyesClosed
                    ? CGRect(
                        x: CGFloat(eyeX) * cell, y: 4.55 * cell,
                        width: cell, height: 0.45 * cell
                    )
                    : CGRect(
                        x: CGFloat(eyeX) * cell, y: 3 * cell,
                        width: cell, height: 2 * cell
                    )
                context.fill(Path(rect), with: .color(Color(hex: Self.eyeHex)))
            }
        }
        // The bob is a whole-pixel nudge, deliberately unanimated — smooth
        // interpolation would break the pixel-art illusion.
        .offset(y: pose.bobbed ? 1 : 0)
    }

    private static let gridSide = 13

    /// The webp's 13×13 body, minus the animated parts (eyes + tail tip are
    /// drawn separately). Tokens are columns of the body ramp (`F…P`), the
    /// lit face band (`H…O`), and the ears (`Q`/`S`); `.` is transparent.
    private static let grid: [[Character]] = [
        "...ABCDE.....",
        "..FABCDEG....",
        ".HIJKLMNOP...",
        "QHIJKLMNOPS..",
        "QHIJKLMNOPS..",
        ".HIJKLMNOP...",
        "..FAKLMEG....",
        "...ABCDE.....",
        "...ABCDEG....",
        "..FABCDEGP...",
        "..FABCDEGP...",
        "..FABCDEGP...",
        "..IJBCDNOP...",
    ].map(Array.init)

    /// The canonical rainbow palette, cell-for-cell from
    /// `unpeel-mascot/mascot-logo.svg`: the body sweeps through the site's
    /// agent accents (teal → green → blue → purple) left to right, the face
    /// band is the same sweep lightened, and the ears take the lightened
    /// gradient ends (warm left, violet right).
    private static let palette: [Character: UInt32] = [
        // body ramp, left → right
        "F": 0x00C4C4, "A": 0x22C38A, "B": 0x43C251, "C": 0x49B5A8,
        "D": 0x4FA8FF, "E": 0x4E92FB, "G": 0x4C7DF7, "P": 0x746FF0,
        // lit face band
        "H": 0x6C9E8E, "I": 0x7FE1E1, "J": 0x90E1C4, "K": 0xA1E0A8,
        "L": 0xA4DAD3, "M": 0xA7D3FF, "N": 0xA6C8FD, "O": 0xA5BEFB,
        // ears
        "Q": 0xECBBAB, "S": 0xCDB0F4,
    ]

    private static let eyeHex: UInt32 = 0x000000
    private static let tailHex: UInt32 = 0x9B61EA

    /// Tail cells that never move (lower bar + the elbow into the body).
    private static let tailBase: [(Int, Int)] = [
        (12, 7), (12, 8), (12, 9), (11, 10), (10, 10),
    ]
    /// Tail tip, resting straight up…
    private static let tailTipUp: [(Int, Int)] = [(12, 5), (12, 6)]
    /// …and mid-flick, bent inward (the webp's alternate frames).
    private static let tailTipFlicked: [(Int, Int)] = [(11, 6), (12, 6)]
}
