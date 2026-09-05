//
//  SidebarSpinners.swift
//  UnpeelNative
//
//  Sidebar activity indicators extracted from SidebarView.swift: the
//  shimmer label sweep and the braille spinner (with their backing NSView
//  layers). Pure presentation, used across the sidebar and other views.
//

import AppKit
import SwiftUI
import UnpeelShared


/// Gradient sweep over a one-line label, replicating the Svelte
/// `.project-name.shimmer` CSS:
///   background: linear-gradient(100deg,
///     color-mix(currentColor 80%) 0%, currentColor 10%,
///     color-mix(currentColor 80%) 20%);
///   background-size: 200% 100%;
///   animation: text-shimmer 1.8s linear infinite;   // 100% 0 → -100% 0
///
/// SwiftUI owns ALL text layout and truncation: the hidden `Text` is the
/// layout authority, and the SAME `Text` (identically truncated) `.mask`s
/// the animated gradient, so the visible glyphs can never mismatch the
/// laid-out frame. (An earlier version let a `CATextLayer` truncate itself
/// independently; at constrained widths — a narrow sidebar with the quick
/// preset strip expanded — that layer rendered nothing and the name
/// vanished, while a plain `Text` truncated to `name…` fine.)
///
/// The gradient itself is a dumb render-server-side `CAGradientLayer` that
/// slides via a repeating `CABasicAnimation` on `transform.translation.x` —
/// zero per-frame app CPU (same discipline as SpinnerLayerView). The bright
/// 100% band occupies stops 10–20%; the rest of the sweep shows the label at
/// 80% color, brighter than the static name's 60% — exactly the CSS's
/// `opacity: 1` + 80% mix.
struct ShimmerLabel: View {
    let text: String
    let color: NSColor

    var body: some View {
        label
            .opacity(0)
            .overlay {
                ShimmerGradientView(color: color)
                    .mask(label)
            }
    }

    private var label: some View {
        Text(text)
            .font(Theme.rowLabelFont)
            .lineLimit(1)
            .truncationMode(.tail)
    }
}

/// The render-server gradient sweep on its own; `ShimmerLabel` clips it to
/// the glyph shape with a SwiftUI `.mask`.
struct ShimmerGradientView: NSViewRepresentable {
    let color: NSColor

    func makeNSView(context _: Context) -> ShimmerGradientLayerView {
        ShimmerGradientLayerView(color: color)
    }

    func updateNSView(_ view: ShimmerGradientLayerView, context _: Context) {
        view.configure(color: color)
    }
}

final class ShimmerGradientLayerView: NSView {
    private var color: NSColor
    private let gradientLayer = CAGradientLayer()

    init(color: NSColor) {
        self.color = color
        super.init(frame: .zero)
        wantsLayer = true

        // Horizontal sweep (the CSS 100deg axis is within a few degrees of
        // horizontal on a 15px-tall line; background-position only moves
        // horizontally anyway).
        gradientLayer.startPoint = CGPoint(x: 0, y: 0.5)
        gradientLayer.endPoint = CGPoint(x: 1, y: 0.5)

        layer?.addSublayer(gradientLayer)
        applyAppearance()
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("not supported") }

    func configure(color: NSColor) {
        guard !color.isEqual(self.color) else { return }
        self.color = color
        applyAppearance()
        needsLayout = true
    }

    private func applyAppearance() {
        // The CSS background REPEATS (background-size 200% tiles a 2w
        // gradient), so the label is always covered. CAGradientLayer can't
        // tile, so the layer carries TWO cycles (4w wide) and the animation
        // translates by one tile (2w) per period: coverage of the label is
        // continuous for the whole loop. One tile's band sits at 10–20%,
        // which is 5–10% (and 55–60%) of the doubled layer.
        // Theme colors are appearance-dynamic and cgColor snapshots at
        // call time — resolve against the view's effective appearance.
        var base = color.cgColor
        var peak = color.cgColor
        effectiveAppearance.performAsCurrentDrawingAppearance {
            base = color.withAlphaComponent(0.8 * color.alphaComponent).cgColor
            peak = color.cgColor
        }
        gradientLayer.colors = [base, peak, base, base, peak, base]
        gradientLayer.locations = [0, 0.05, 0.10, 0.5, 0.55, 0.60]
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyAppearance()
    }

    override func layout() {
        super.layout()
        let scale = window?.backingScaleFactor ?? 2
        gradientLayer.contentsScale = scale
        // Layer geometry must not animate with the default implicit 0.25s
        // (rows resize with the sidebar).
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        // Two 2w tiles (see applyAppearance), parked so the label sits in
        // the layer's right half; the animation slides one tile rightwards.
        gradientLayer.frame = CGRect(
            x: -bounds.width * 3, y: 0,
            width: bounds.width * 4, height: bounds.height
        )
        CATransaction.commit()
        restartAnimation()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil {
            needsLayout = true
        } else {
            gradientLayer.removeAnimation(forKey: "shimmer")
        }
    }

    private func restartAnimation() {
        guard window != nil, bounds.width > 0 else { return }
        // background-position 100% → -100% over 1.8s linear infinite:
        // with a 2w gradient anchored at x = -w, that is a translation
        // from 0 to +2w.
        let animation = CABasicAnimation(keyPath: "transform.translation.x")
        animation.fromValue = 0
        animation.toValue = bounds.width * 2
        animation.duration = 1.8
        animation.timingFunction = CAMediaTimingFunction(name: .linear)
        animation.repeatCount = .infinity
        gradientLayer.removeAnimation(forKey: "shimmer")
        gradientLayer.add(animation, forKey: "shimmer")
    }
}

// MARK: - Busy spinner (braille glyphs, 120ms/frame; DESIGN.md §5)

/// Core-Animation-driven braille spinner. The original SwiftUI
/// `TimelineView(.periodic)` kept the UpdateCycle display-link driver alive
/// and re-rendered the hosting view continuously (~13-15% CPU with a single
/// visible spinner, confirmed by sampling); even a Timer-driven Text swap
/// cost ~4-5% in whole-view-graph layout per tick. Instead the 10 frames
/// are pre-rendered once and cycled by a repeating `CAKeyframeAnimation` on
/// `layer.contents`, which runs entirely on the render server — zero
/// per-frame work in the app process. The view unmounts when the row stops
/// being busy, which removes the animation.
struct BrailleSpinner: View {
    let color: Color

    var body: some View {
        SpinnerLayerRepresentable(color: NSColor(color))
            .frame(width: 16, height: 16)
            .accessibilityHidden(true)
    }
}

private struct SpinnerLayerRepresentable: NSViewRepresentable {
    let color: NSColor

    func makeNSView(context _: Context) -> SpinnerLayerView {
        SpinnerLayerView(color: color)
    }

    func updateNSView(_ view: SpinnerLayerView, context _: Context) {
        view.setColor(color)
    }
}

final class SpinnerLayerView: NSView {
    private var color: NSColor

    init(color: NSColor) {
        self.color = color
        super.init(frame: NSRect(x: 0, y: 0, width: 16, height: 16))
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("not supported") }

    func setColor(_ color: NSColor) {
        guard !self.color.isEqual(color) else { return }
        self.color = color
        if window != nil { restartAnimation() }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil {
            restartAnimation()
        } else {
            layer?.removeAnimation(forKey: "spinner")
        }
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        if window != nil { restartAnimation() }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        if window != nil { restartAnimation() }
    }

    private func restartAnimation() {
        guard let layer else { return }
        let scale = window?.backingScaleFactor ?? 2
        // Theme colors are appearance-dynamic; flatten against the view's
        // effective appearance so the frame cache keys on resolved sRGB.
        var resolved = color
        effectiveAppearance.performAsCurrentDrawingAppearance {
            resolved = color.usingColorSpace(.sRGB) ?? color
        }
        let frames = Self.frames(color: resolved, scale: scale)
        guard !frames.isEmpty else { return }
        layer.contentsScale = scale
        // Static contents so non-animated renders (snapshots) show a frame.
        layer.contents = frames[0]
        let animation = CAKeyframeAnimation(keyPath: "contents")
        animation.values = frames
        animation.calculationMode = .discrete
        animation.duration = Theme.spinnerInterval * Double(frames.count)
        animation.repeatCount = .infinity
        layer.removeAnimation(forKey: "spinner")
        layer.add(animation, forKey: "spinner")
    }

    /// Pre-rendered frame images, cached per (color, scale). The glyph is
    /// drawn with the same metrics as the old SwiftUI Text: 14.7pt bold
    /// monospaced, glow shadow at 55% color, radius 3.
    private static var frameCache: [String: [CGImage]] = [:]

    private static func frames(color: NSColor, scale: CGFloat) -> [CGImage] {
        let rgb = color.usingColorSpace(.sRGB) ?? color
        let key = String(
            format: "%.3f-%.3f-%.3f-%.3f@%.1f",
            rgb.redComponent, rgb.greenComponent, rgb.blueComponent,
            rgb.alphaComponent, scale
        )
        if let cached = frameCache[key] { return cached }

        let side: CGFloat = 16
        let pixels = Int(side * scale)
        let shadow = NSShadow()
        shadow.shadowColor = color.withAlphaComponent(0.55)
        shadow.shadowBlurRadius = 3
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 14.7, weight: .bold),
            .foregroundColor: color,
            .shadow: shadow,
        ]

        var images: [CGImage] = []
        for glyph in Theme.spinnerFrames {
            guard let context = CGContext(
                data: nil, width: pixels, height: pixels,
                bitsPerComponent: 8, bytesPerRow: 0,
                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { continue }
            context.scaleBy(x: scale, y: scale)

            let graphics = NSGraphicsContext(cgContext: context, flipped: false)
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = graphics
            let text = NSAttributedString(string: glyph, attributes: attributes)
            let bounds = text.boundingRect(
                with: NSSize(width: side, height: side), options: []
            )
            text.draw(at: NSPoint(
                x: (side - bounds.width) / 2,
                y: (side - bounds.height) / 2
            ))
            NSGraphicsContext.restoreGraphicsState()

            if let image = context.makeImage() {
                images.append(image)
            }
        }
        frameCache[key] = images
        return images
    }
}

// MARK: - Footer (Sidebar.svelte:562-597)
