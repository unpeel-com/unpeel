//
//  SessionGalleryMarkup.swift
//  UnpeelNative
//
//  Arrow + crop markup for the desktop session gallery's detail view — the
//  desktop twin of the iOS gallery markup (ArrowMarkup/ArrowGeometry in
//  BrowserGalleryPanel.swift / ImageAnnotationView.swift; keep the palette,
//  stroke formulas, and arrow geometry in step so annotations look the same
//  from both apps). Arrows are stored in NORMALIZED image coordinates
//  (0…1, top-left origin) so they're resolution-independent: the on-screen
//  canvas maps them to the fitted rect, and `flatten` maps them to the
//  image's native pixels.
//

import AppKit
import SwiftUI

struct GalleryArrow: Identifiable, Equatable {
    let id = UUID()
    var start: CGPoint
    var end: CGPoint
    var colorHex: UInt32
}

enum GalleryMarkup {
    /// Same palette as the iOS gallery markup.
    static let palette: [UInt32] = [0xEF4444, 0xEAB308, 0x22C55E, 0x3B82F6, 0xFFFFFF]

    /// Stroke metrics relative to the surface being drawn on (canvas points
    /// on screen, pixels on flatten) — identical formulas to iOS ArrowMarkup.
    static func lineWidth(for width: CGFloat) -> CGFloat { max(2.5, width * 0.009) }
    static func headLength(for width: CGFloat) -> CGFloat { max(12, width * 0.038) }

    static func color(_ hex: UInt32) -> Color {
        Color(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }

    static func cgColor(_ hex: UInt32) -> CGColor {
        CGColor(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1
        )
    }

    /// Line + two head strokes — twin of iOS `ArrowGeometry.path`.
    static func arrowPath(from start: CGPoint, to end: CGPoint, headLength: CGFloat) -> Path {
        var path = Path()
        path.move(to: start)
        path.addLine(to: end)
        let angle = atan2(end.y - start.y, end.x - start.x)
        let spread: CGFloat = .pi / 7
        let left = CGPoint(
            x: end.x - headLength * cos(angle - spread),
            y: end.y - headLength * sin(angle - spread)
        )
        let right = CGPoint(
            x: end.x - headLength * cos(angle + spread),
            y: end.y - headLength * sin(angle + spread)
        )
        path.move(to: end)
        path.addLine(to: left)
        path.move(to: end)
        path.addLine(to: right)
        return path
    }

    /// Aspect-fit of `image` inside `bounds`.
    static func fittedSize(_ image: CGSize, in bounds: CGSize) -> CGSize {
        guard image.width > 0, image.height > 0, bounds.width > 0, bounds.height > 0 else {
            return bounds
        }
        let scale = min(bounds.width / image.width, bounds.height / image.height)
        return CGSize(width: image.width * scale, height: image.height * scale)
    }

    /// Composite the arrows onto `image` at its native resolution.
    static func flatten(_ image: CGImage, arrows: [GalleryArrow]) -> CGImage {
        guard !arrows.isEmpty else { return image }
        let width = image.width
        let height = image.height
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(
                  data: nil,
                  width: width,
                  height: height,
                  bitsPerComponent: 8,
                  bytesPerRow: 0,
                  space: space,
                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
              )
        else { return image }
        let w = CGFloat(width)
        let h = CGFloat(height)
        context.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        context.setLineCap(.round)
        context.setLineJoin(.round)
        context.setLineWidth(lineWidth(for: w))
        let head = headLength(for: w)
        for arrow in arrows {
            // Normalized coords are top-left origin; CG contexts are
            // bottom-left, so flip y.
            let start = CGPoint(x: arrow.start.x * w, y: h - arrow.start.y * h)
            let end = CGPoint(x: arrow.end.x * w, y: h - arrow.end.y * h)
            context.setStrokeColor(cgColor(arrow.colorHex))
            context.addPath(arrowPath(from: start, to: end, headLength: head).cgPath)
            context.strokePath()
        }
        return context.makeImage() ?? image
    }

    /// Crop by a normalized (top-left origin) rect. `CGImage.cropping` takes
    /// pixel coordinates with the same top-left origin, so no flip here.
    static func crop(_ image: CGImage, normalizedRect rect: CGRect) -> CGImage? {
        let w = CGFloat(image.width)
        let h = CGFloat(image.height)
        let pixelRect = CGRect(
            x: (rect.minX * w).rounded(.down),
            y: (rect.minY * h).rounded(.down),
            width: max(1, (rect.width * w).rounded()),
            height: max(1, (rect.height * h).rounded())
        )
        return image.cropping(to: pixelRect)
    }

    static func pngData(_ image: CGImage) -> Data? {
        let rep = NSBitmapImageRep(cgImage: image)
        return rep.representation(using: .png, properties: [:])
    }
}

/// The detail view's image surface: aspect-fitted image + arrow canvas +
/// crop-selection overlay, with one drag gesture the parent interprets per
/// mode. Reports drag points in normalized image coordinates.
struct GalleryAnnotatableImage: View {
    let image: NSImage
    let pixelSize: CGSize
    let arrows: [GalleryArrow]
    let liveArrow: GalleryArrow?
    /// Normalized crop selection to show, or nil.
    let cropSelection: CGRect?
    /// Whether drags should be captured (arrow or crop mode active).
    let interactive: Bool
    /// (normalized start, normalized current, fitted size, ended)
    let onDrag: (CGPoint, CGPoint, CGSize, Bool) -> Void

    var body: some View {
        GeometryReader { geo in
            let fitted = GalleryMarkup.fittedSize(pixelSize, in: geo.size)
            ZStack {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                arrowCanvas
                if let cropSelection {
                    GalleryCropOverlay(rect: cropSelection)
                        .allowsHitTesting(false)
                }
            }
            .frame(width: fitted.width, height: fitted.height)
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            .contentShape(Rectangle())
            .gesture(dragGesture(fitted: fitted), including: interactive ? .all : .subviews)
            .position(x: geo.size.width / 2, y: geo.size.height / 2)
        }
    }

    private var arrowCanvas: some View {
        Canvas { context, size in
            let lineWidth = GalleryMarkup.lineWidth(for: size.width)
            let head = GalleryMarkup.headLength(for: size.width)
            var all = arrows
            if let liveArrow { all.append(liveArrow) }
            for arrow in all {
                let path = GalleryMarkup.arrowPath(
                    from: CGPoint(x: arrow.start.x * size.width, y: arrow.start.y * size.height),
                    to: CGPoint(x: arrow.end.x * size.width, y: arrow.end.y * size.height),
                    headLength: head
                )
                context.stroke(
                    path,
                    with: .color(GalleryMarkup.color(arrow.colorHex)),
                    style: StrokeStyle(lineWidth: lineWidth, lineCap: .round, lineJoin: .round)
                )
            }
        }
        .allowsHitTesting(false)
    }

    private func dragGesture(fitted: CGSize) -> some Gesture {
        DragGesture(minimumDistance: 2)
            .onChanged { value in
                onDrag(
                    normalize(value.startLocation, in: fitted),
                    normalize(value.location, in: fitted),
                    fitted,
                    false
                )
            }
            .onEnded { value in
                onDrag(
                    normalize(value.startLocation, in: fitted),
                    normalize(value.location, in: fitted),
                    fitted,
                    true
                )
            }
    }

    private func normalize(_ point: CGPoint, in fitted: CGSize) -> CGPoint {
        CGPoint(
            x: min(max(point.x / max(fitted.width, 1), 0), 1),
            y: min(max(point.y / max(fitted.height, 1), 0), 1)
        )
    }
}

/// Dim everything outside the selection, hairline around it.
private struct GalleryCropOverlay: View {
    let rect: CGRect

    var body: some View {
        GeometryReader { geo in
            let r = CGRect(
                x: rect.minX * geo.size.width,
                y: rect.minY * geo.size.height,
                width: rect.width * geo.size.width,
                height: rect.height * geo.size.height
            )
            Path { path in
                path.addRect(CGRect(origin: .zero, size: geo.size))
                path.addRect(r)
            }
            .fill(Color.black.opacity(0.45), style: FillStyle(eoFill: true))
            Path { path in
                path.addRect(r)
            }
            .stroke(.white.opacity(0.9), lineWidth: 1.5)
        }
    }
}
