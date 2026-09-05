import SwiftUI
#if canImport(PencilKit)
import PencilKit
#endif
#if os(iOS)
import UIKit
#endif

/// Freehand annotation editor over an image: pen / marker / eraser, colors,
/// and undo, all from PencilKit's system tool picker. Returns the flattened
/// image (original + drawing) so the caller can attach or save it.
///
/// Built on PencilKit deliberately: it exists on **both iOS (13+) and macOS
/// (11+)**, so this editor is a candidate to lift into a shared component for
/// the desktop app. Only two pieces are platform-specific and isolated below —
/// the `PKCanvasView` representable wrapper (UIView vs NSView) and the image
/// flatten (`UIGraphicsImageRenderer` vs `NSImage`). Everything else (layout,
/// fitted-rect math, scale-to-native) ports unchanged.
struct ImageAnnotationView: View {
    let image: UIImage
    let onDone: (UIImage) -> Void
    let onCancel: () -> Void

    @State private var canvas = PKCanvasView()
    @State private var toolPicker = PKToolPicker()
    /// The on-screen size of the fitted image = the canvas coordinate space.
    /// Captured so the flatten can scale strokes up to native resolution.
    @State private var canvasSize: CGSize = .zero

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            GeometryReader { geo in
                let fitted = Self.fittedSize(image.size, in: geo.size)
                ZStack {
                    Image(uiImage: image)
                        .resizable()
                        .frame(width: fitted.width, height: fitted.height)
                    AnnotationCanvas(canvas: canvas, toolPicker: toolPicker)
                        .frame(width: fitted.width, height: fitted.height)
                }
                .frame(width: geo.size.width, height: geo.size.height)
                .onAppear { canvasSize = fitted }
                .onChange(of: fitted) { canvasSize = $0 }
            }

            VStack {
                HStack(spacing: 12) {
                    toolbarButton("Cancel") { onCancel() }
                    Spacer()
                    iconButton("arrow.uturn.backward", label: "Undo") {
                        canvas.undoManager?.undo()
                    }
                    iconButton("trash", label: "Clear") {
                        canvas.drawing = PKDrawing()
                    }
                    toolbarButton("Done", prominent: true) {
                        onDone(flattened())
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 14)
                Spacer()
            }
        }
        .environment(\.colorScheme, .dark)
    }

    private func toolbarButton(_ title: String, prominent: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 15, weight: .semibold))
                .padding(.horizontal, 16)
                .frame(height: 40)
                .background(
                    prominent ? Color.accentColor : Color.white.opacity(0.14),
                    in: Capsule()
                )
                .foregroundStyle(.white)
        }
    }

    private func iconButton(_ systemImage: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 40, height: 40)
                .background(.ultraThinMaterial, in: Circle())
        }
        .accessibilityLabel(label)
    }

    /// Aspect-fit the image inside `bounds` — the displayed rect the canvas
    /// overlays exactly.
    static func fittedSize(_ imageSize: CGSize, in bounds: CGSize) -> CGSize {
        guard imageSize.width > 0, imageSize.height > 0,
              bounds.width > 0, bounds.height > 0
        else { return bounds }
        let scale = min(bounds.width / imageSize.width, bounds.height / imageSize.height)
        return CGSize(width: imageSize.width * scale, height: imageSize.height * scale)
    }

    /// Composite the original image with the drawing, rendered at the image's
    /// **native** resolution (strokes are scaled up from canvas points), so
    /// attaching the result isn't a low-res downgrade.
    private func flattened() -> UIImage {
        let target = image.size
        guard canvasSize.width > 0, canvasSize.height > 0 else { return image }
        let sx = target.width / canvasSize.width
        let sy = target.height / canvasSize.height

        var scaled = canvas.drawing
        scaled.transform(using: CGAffineTransform(scaleX: sx, y: sy))

        let format = UIGraphicsImageRendererFormat()
        format.scale = image.scale
        format.opaque = false
        return UIGraphicsImageRenderer(size: target, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: target))
            scaled.image(from: CGRect(origin: .zero, size: target), scale: image.scale)
                .draw(in: CGRect(origin: .zero, size: target))
        }
    }
}

/// Shared arrow geometry (line + two head strokes), so the on-screen preview
/// and the flattened output are pixel-identical.
enum ArrowGeometry {
    static func path(from start: CGPoint, to end: CGPoint, headLength: CGFloat) -> Path {
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
}

/// Draw arrows onto an image by dragging start → end. Purpose-built markup
/// (replaces freehand pencil): a few colors, undo/clear, and a flatten that
/// scales strokes up to the image's native resolution. Arrows live in screen
/// points relative to the fitted image, mapped to pixels on export.
struct ImageArrowMarkupView: View {
    let image: UIImage
    let onDone: (UIImage) -> Void
    let onCancel: () -> Void

    private struct Arrow: Identifiable {
        let id = UUID()
        var start: CGPoint
        var end: CGPoint
        var color: Color
    }

    @State private var arrows: [Arrow] = []
    @State private var current: Arrow?
    @State private var color: Color = .red
    /// The fitted image rect in the drawing coordinate space — its origin
    /// offsets screen points into image space for the flatten.
    @State private var fittedRect: CGRect = .zero

    private static let onScreenLineWidth: CGFloat = 4
    private static let onScreenHeadLength: CGFloat = 18
    private let palette: [Color] = [.red, .yellow, .green, .blue, .white]
    private let space = "arrowMarkup"

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            GeometryReader { geo in
                let fitted = ImageAnnotationView.fittedSize(image.size, in: geo.size)
                let rect = CGRect(
                    x: (geo.size.width - fitted.width) / 2,
                    y: (geo.size.height - fitted.height) / 2,
                    width: fitted.width,
                    height: fitted.height
                )
                ZStack(alignment: .topLeading) {
                    Image(uiImage: image)
                        .resizable()
                        .frame(width: fitted.width, height: fitted.height)
                        .position(x: geo.size.width / 2, y: geo.size.height / 2)

                    ForEach(arrows) { arrow in
                        ArrowGeometry
                            .path(from: arrow.start, to: arrow.end, headLength: Self.onScreenHeadLength)
                            .stroke(
                                arrow.color,
                                style: StrokeStyle(lineWidth: Self.onScreenLineWidth, lineCap: .round, lineJoin: .round)
                            )
                    }
                    if let current {
                        ArrowGeometry
                            .path(from: current.start, to: current.end, headLength: Self.onScreenHeadLength)
                            .stroke(
                                current.color,
                                style: StrokeStyle(lineWidth: Self.onScreenLineWidth, lineCap: .round, lineJoin: .round)
                            )
                    }
                }
                .frame(width: geo.size.width, height: geo.size.height)
                .contentShape(Rectangle())
                .coordinateSpace(name: space)
                .gesture(
                    DragGesture(minimumDistance: 4, coordinateSpace: .named(space))
                        .onChanged { value in
                            current = Arrow(
                                start: clamp(value.startLocation, to: rect),
                                end: clamp(value.location, to: rect),
                                color: color
                            )
                        }
                        .onEnded { value in
                            let start = clamp(value.startLocation, to: rect)
                            let end = clamp(value.location, to: rect)
                            if hypot(end.x - start.x, end.y - start.y) > 8 {
                                arrows.append(Arrow(start: start, end: end, color: color))
                            }
                            current = nil
                        }
                )
                .onAppear { fittedRect = rect }
                .onChange(of: rect) { fittedRect = $0 }
            }

            VStack {
                HStack(spacing: 12) {
                    capsuleButton("Cancel") { onCancel() }
                    Spacer()
                    circleButton("arrow.uturn.backward", label: "Undo") {
                        if !arrows.isEmpty { arrows.removeLast() }
                    }
                    circleButton("trash", label: "Clear") { arrows.removeAll() }
                    capsuleButton("Done", prominent: true) { onDone(flattened()) }
                }
                .padding(.horizontal, 16)
                .padding(.top, 14)

                Spacer()

                HStack(spacing: 14) {
                    ForEach(palette, id: \.self) { swatch in
                        Button { color = swatch } label: {
                            Circle()
                                .fill(swatch)
                                .frame(width: 28, height: 28)
                                .overlay(
                                    Circle().strokeBorder(
                                        .white.opacity(color == swatch ? 0.95 : 0.25),
                                        lineWidth: color == swatch ? 3 : 1
                                    )
                                )
                        }
                        .accessibilityLabel("Color")
                    }
                }
                .padding(.vertical, 12)
                .padding(.horizontal, 18)
                .background(.ultraThinMaterial, in: Capsule())
                .padding(.bottom, 24)
            }
        }
        .environment(\.colorScheme, .dark)
    }

    private func clamp(_ point: CGPoint, to rect: CGRect) -> CGPoint {
        CGPoint(
            x: min(max(point.x, rect.minX), rect.maxX),
            y: min(max(point.y, rect.minY), rect.maxY)
        )
    }

    private func flattened() -> UIImage {
        guard fittedRect.width > 0, !arrows.isEmpty else { return image }
        let scale = image.size.width / fittedRect.width
        let format = UIGraphicsImageRendererFormat()
        format.scale = image.scale
        format.opaque = false
        return UIGraphicsImageRenderer(size: image.size, format: format).image { context in
            image.draw(in: CGRect(origin: .zero, size: image.size))
            let cg = context.cgContext
            cg.setLineCap(.round)
            cg.setLineJoin(.round)
            cg.setLineWidth(Self.onScreenLineWidth * scale)
            for arrow in arrows {
                let start = CGPoint(
                    x: (arrow.start.x - fittedRect.minX) * scale,
                    y: (arrow.start.y - fittedRect.minY) * scale
                )
                let end = CGPoint(
                    x: (arrow.end.x - fittedRect.minX) * scale,
                    y: (arrow.end.y - fittedRect.minY) * scale
                )
                cg.setStrokeColor(UIColor(arrow.color).cgColor)
                cg.addPath(
                    ArrowGeometry
                        .path(from: start, to: end, headLength: Self.onScreenHeadLength * scale)
                        .cgPath
                )
                cg.strokePath()
            }
        }
    }

    private func capsuleButton(_ title: String, prominent: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 15, weight: .semibold))
                .padding(.horizontal, 16)
                .frame(height: 40)
                .background(prominent ? Color.accentColor : Color.white.opacity(0.14), in: Capsule())
                .foregroundStyle(.white)
        }
    }

    private func circleButton(_ systemImage: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 40, height: 40)
                .background(.ultraThinMaterial, in: Circle())
        }
        .accessibilityLabel(label)
    }
}

/// Crop an image to an adjustable rectangle (four corner handles + drag to
/// move). The crop rect lives in screen points over the fitted image; on Done
/// it maps to the image's native pixels and renders the cropped result.
struct ImageCropView: View {
    let image: UIImage
    let onDone: (UIImage) -> Void
    let onCancel: () -> Void

    @State private var fittedRect: CGRect = .zero
    @State private var cropRect: CGRect = .zero
    private let space = "imageCrop"
    private let handle: CGFloat = 28
    private let minSize: CGFloat = 44

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            GeometryReader { geo in
                let fitted = ImageAnnotationView.fittedSize(image.size, in: geo.size)
                let rect = CGRect(
                    x: (geo.size.width - fitted.width) / 2,
                    y: (geo.size.height - fitted.height) / 2,
                    width: fitted.width,
                    height: fitted.height
                )
                ZStack(alignment: .topLeading) {
                    Image(uiImage: image)
                        .resizable()
                        .frame(width: fitted.width, height: fitted.height)
                        .position(x: geo.size.width / 2, y: geo.size.height / 2)

                    if cropRect != .zero {
                        cropOverlay
                    }
                }
                .frame(width: geo.size.width, height: geo.size.height)
                .coordinateSpace(name: space)
                .onAppear {
                    fittedRect = rect
                    if cropRect == .zero { cropRect = rect }
                }
                .onChange(of: rect) { newRect in
                    // Keep the crop proportional if the container resizes.
                    fittedRect = newRect
                    cropRect = newRect
                }
            }

            VStack {
                HStack(spacing: 12) {
                    capsuleButton("Cancel") { onCancel() }
                    Spacer()
                    circleButton("arrow.counterclockwise", label: "Reset") {
                        cropRect = fittedRect
                    }
                    capsuleButton("Done", prominent: true) { onDone(cropped()) }
                }
                .padding(.horizontal, 16)
                .padding(.top, 14)
                Spacer()
            }
        }
        .environment(\.colorScheme, .dark)
    }

    private var cropOverlay: some View {
        ZStack(alignment: .topLeading) {
            // Dim everything outside the crop rect.
            Rectangle()
                .fill(.black.opacity(0.55))
                .reverseMask {
                    Rectangle()
                        .frame(width: cropRect.width, height: cropRect.height)
                        .position(x: cropRect.midX, y: cropRect.midY)
                }
                .allowsHitTesting(false)

            Rectangle()
                .strokeBorder(.white.opacity(0.9), lineWidth: 1.5)
                .frame(width: cropRect.width, height: cropRect.height)
                .position(x: cropRect.midX, y: cropRect.midY)
                .contentShape(Rectangle())
                .gesture(moveGesture)

            cornerHandle(.topLeft)
            cornerHandle(.topRight)
            cornerHandle(.bottomLeft)
            cornerHandle(.bottomRight)
        }
    }

    private enum Corner { case topLeft, topRight, bottomLeft, bottomRight }

    private func cornerPoint(_ corner: Corner) -> CGPoint {
        switch corner {
        case .topLeft: return CGPoint(x: cropRect.minX, y: cropRect.minY)
        case .topRight: return CGPoint(x: cropRect.maxX, y: cropRect.minY)
        case .bottomLeft: return CGPoint(x: cropRect.minX, y: cropRect.maxY)
        case .bottomRight: return CGPoint(x: cropRect.maxX, y: cropRect.maxY)
        }
    }

    private func cornerHandle(_ corner: Corner) -> some View {
        let point = cornerPoint(corner)
        return Circle()
            .fill(.white)
            .frame(width: 16, height: 16)
            .overlay(Circle().strokeBorder(.black.opacity(0.25), lineWidth: 1))
            .frame(width: handle, height: handle)
            .contentShape(Circle())
            .position(x: point.x, y: point.y)
            .gesture(
                DragGesture(coordinateSpace: .named(space))
                    .onChanged { value in resize(corner, to: value.location) }
            )
    }

    private func resize(_ corner: Corner, to location: CGPoint) {
        let bounds = fittedRect
        let x = min(max(location.x, bounds.minX), bounds.maxX)
        let y = min(max(location.y, bounds.minY), bounds.maxY)
        var newRect = cropRect
        switch corner {
        case .topLeft:
            newRect.origin.x = min(x, cropRect.maxX - minSize)
            newRect.origin.y = min(y, cropRect.maxY - minSize)
            newRect.size.width = cropRect.maxX - newRect.origin.x
            newRect.size.height = cropRect.maxY - newRect.origin.y
        case .topRight:
            newRect.origin.y = min(y, cropRect.maxY - minSize)
            newRect.size.width = max(x - cropRect.minX, minSize)
            newRect.size.height = cropRect.maxY - newRect.origin.y
        case .bottomLeft:
            newRect.origin.x = min(x, cropRect.maxX - minSize)
            newRect.size.width = cropRect.maxX - newRect.origin.x
            newRect.size.height = max(y - cropRect.minY, minSize)
        case .bottomRight:
            newRect.size.width = max(x - cropRect.minX, minSize)
            newRect.size.height = max(y - cropRect.minY, minSize)
        }
        cropRect = newRect
    }

    private var moveGesture: some Gesture {
        DragGesture(coordinateSpace: .named(space))
            .onChanged { value in
                var newRect = cropRect
                newRect.origin.x += value.translation.width - dragAccumulator.width
                newRect.origin.y += value.translation.height - dragAccumulator.height
                newRect.origin.x = min(max(newRect.origin.x, fittedRect.minX), fittedRect.maxX - newRect.width)
                newRect.origin.y = min(max(newRect.origin.y, fittedRect.minY), fittedRect.maxY - newRect.height)
                cropRect = newRect
                dragAccumulator = value.translation
            }
            .onEnded { _ in dragAccumulator = .zero }
    }

    @State private var dragAccumulator: CGSize = .zero

    private func cropped() -> UIImage {
        guard fittedRect.width > 0, cropRect.width > 4, cropRect.height > 4 else { return image }
        let scale = image.size.width / fittedRect.width
        let cropInImage = CGRect(
            x: (cropRect.minX - fittedRect.minX) * scale,
            y: (cropRect.minY - fittedRect.minY) * scale,
            width: cropRect.width * scale,
            height: cropRect.height * scale
        )
        let format = UIGraphicsImageRendererFormat()
        format.scale = image.scale
        format.opaque = false
        return UIGraphicsImageRenderer(size: cropInImage.size, format: format).image { _ in
            image.draw(at: CGPoint(x: -cropInImage.origin.x, y: -cropInImage.origin.y))
        }
    }

    private func capsuleButton(_ title: String, prominent: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 15, weight: .semibold))
                .padding(.horizontal, 16)
                .frame(height: 40)
                .background(prominent ? Color.accentColor : Color.white.opacity(0.14), in: Capsule())
                .foregroundStyle(.white)
        }
    }

    private func circleButton(_ systemImage: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 40, height: 40)
                .background(.ultraThinMaterial, in: Circle())
        }
        .accessibilityLabel(label)
    }
}

/// Cut a hole in a view using another view as an inverted mask (for the crop
/// dimming: darken everything except the crop rect).
private extension View {
    func reverseMask<Mask: View>(@ViewBuilder _ mask: () -> Mask) -> some View {
        self.mask {
            ZStack {
                Rectangle()
                mask().blendMode(.destinationOut)
            }
            .compositingGroup()
        }
    }
}

#if os(iOS)
/// The PencilKit drawing surface, transparent so the image shows through.
/// (Platform-specific: the macOS lift is the NSViewRepresentable mirror.)
private struct AnnotationCanvas: UIViewRepresentable {
    let canvas: PKCanvasView
    let toolPicker: PKToolPicker

    func makeUIView(context: Context) -> PKCanvasView {
        canvas.drawingPolicy = .anyInput        // finger + Apple Pencil
        canvas.backgroundColor = .clear
        canvas.isOpaque = false
        canvas.isScrollEnabled = false
        toolPicker.addObserver(canvas)
        toolPicker.setVisible(true, forFirstResponder: canvas)
        // Must be first responder for the tool picker to appear.
        DispatchQueue.main.async { canvas.becomeFirstResponder() }
        return canvas
    }

    func updateUIView(_ uiView: PKCanvasView, context: Context) {}
}
#endif
