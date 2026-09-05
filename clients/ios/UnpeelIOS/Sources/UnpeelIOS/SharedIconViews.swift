import CoreGraphics
import Foundation
import SwiftUI
import UnpeelShared

#if os(iOS)
import UIKit
#endif

struct SharedChromeIconView: View {
    let icon: UnpeelChromeIcon
    var size: CGFloat = 16

    var body: some View {
        SharedSVGIconView(
            svg: icon.svgSource,
            fallbackSystemName: fallbackSystemName,
            size: size
        )
        .rotationEffect(.degrees(icon.rotationDegrees))
    }

    private var fallbackSystemName: String {
        switch icon {
        case .folderClosed, .folderOpen: return "folder"
        case .branch: return "arrow.triangle.branch"
        case .pin: return "pin.fill"
        case .plus: return "plus"
        case .sidebarToggle: return "sidebar.left"
        case .bell: return "bell.fill"
        case .gallery: return "photo.on.rectangle.angled"
        }
    }
}

struct SharedToolIconView: View {
    let icon: UnpeelToolIcon
    var size: CGFloat = 16

    init(providerID: String?, command: String, size: CGFloat = 16) {
        icon = UnpeelToolIcon.resolving(providerID: providerID, command: command)
        self.size = size
    }

    var body: some View {
        SharedSVGIconView(
            svg: icon.svgSource,
            fallbackSystemName: icon.fallbackSystemName,
            size: size,
            isTemplate: icon.isTemplate
        )
    }
}

/// The Unpeel brand mark (two stacked panels with lit gradient rims), rendered
/// from the canonical logo SVG via the same rasterizer the chrome icons use.
/// White artwork — meant for the app's dark chrome.
struct UnpeelBrandLogo: View {
    var size: CGFloat = 64

    #if os(iOS)
    @Environment(\.displayScale) private var displayScale
    #endif

    var body: some View {
        #if os(iOS)
        if let image = SVGIconRasterizer.image(svg: Self.svg, pointSize: size, scale: displayScale) {
            Image(uiImage: image)
                .frame(width: size, height: size)
                .allowsHitTesting(false)
        } else {
            fallback
        }
        #else
        fallback
        #endif
    }

    private var fallback: some View {
        RoundedRectangle(cornerRadius: size * 0.22, style: .continuous)
            .fill(.white.opacity(0.9))
            .frame(width: size, height: size)
    }

    // Generated from apps/website/app/components/Logo.tsx (scripts/logo-source.svg);
    // gradient ids made static and attributes normalized to standard SVG.
    private static let svg = ##"""
    <svg viewBox="0 0 446 446" fill="none"><path d="M1.62461e-05 37.1667C1.80406e-05 16.6401 16.6401 -1.19583e-06 37.1667 0L408.833 3.24921e-05C429.36 3.42866e-05 446 16.6401 446 37.1667L446 223L345.65 223V118.933C345.65 108.67 337.33 100.35 327.067 100.35L118.933 100.35C108.67 100.35 100.35 108.67 100.35 118.933L100.35 223H0L1.62461e-05 37.1667Z" fill="#D9D9D9" fill-opacity="0.2"/><path d="M37.1667 1.85833L37.1667 0L408.833 3.24921e-05V1.85837L37.1667 1.85833ZM347.508 118.933V221.142L444.142 221.142L444.142 37.1667C444.142 17.6665 428.334 1.85837 408.833 1.85837V3.24921e-05C429.36 3.42866e-05 446 16.6401 446 37.1667L446 223L345.65 223V118.933C345.65 108.67 337.33 100.35 327.067 100.35L118.933 100.35C108.67 100.35 100.35 108.67 100.35 118.933L100.35 223H0L1.62461e-05 37.1667C1.80406e-05 16.6401 16.6401 -1.19583e-06 37.1667 0L37.1667 1.85833C17.6664 1.85833 1.85835 17.6664 1.85835 37.1667L1.85833 221.142H98.4917V118.933C98.4917 107.644 107.644 98.4917 118.933 98.4917L327.067 98.4917C338.356 98.4917 347.508 107.644 347.508 118.933Z" fill="url(#logoBack)"/><path d="M408.833 446C429.36 446 446 429.36 446 408.833V223C446 212.737 437.68 204.417 427.417 204.417H364.233C353.97 204.417 345.65 212.737 345.65 223V327.067C345.65 337.33 337.33 345.65 327.067 345.65H118.933C108.67 345.65 100.35 337.33 100.35 327.067V223C100.35 212.737 92.03 204.417 81.7667 204.417H18.5833C8.32004 204.417 0 212.737 0 223V408.833C0 429.36 16.6401 446 37.1667 446H408.833Z" fill="white"/><path d="M408.833 444.142V446H37.1667V444.142H408.833ZM444.142 408.833V223C444.142 213.763 436.654 206.275 427.417 206.275H364.233C354.996 206.275 347.508 213.763 347.508 223V327.067C347.508 338.356 338.356 347.508 327.067 347.508H118.933C107.644 347.508 98.4917 338.356 98.4917 327.067V223C98.4917 213.763 91.0036 206.275 81.7667 206.275H18.5833C9.34637 206.275 1.85833 213.763 1.85833 223V408.833C1.85833 428.334 17.6664 444.142 37.1667 444.142V446C16.6401 446 0 429.36 0 408.833V223C0 212.737 8.32004 204.417 18.5833 204.417H81.7667C92.03 204.417 100.35 212.737 100.35 223V327.067C100.35 337.33 108.67 345.65 118.933 345.65H327.067C337.33 345.65 345.65 337.33 345.65 327.067V223C345.65 212.737 353.97 204.417 364.233 204.417H427.417C437.68 204.417 446 212.737 446 223V408.833C446 429.36 429.36 446 408.833 446V444.142C428.334 444.142 444.142 428.334 444.142 408.833Z" fill="url(#logoFront)" fill-opacity="0.2"/><defs><linearGradient id="logoBack" x1="223" y1="204.417" x2="301.979" y2="476.663" gradientUnits="userSpaceOnUse"><stop stop-color="white" stop-opacity="0.05"/><stop offset="1" stop-color="white" stop-opacity="0.02"/></linearGradient><linearGradient id="logoFront" x1="223" y1="204.417" x2="301.979" y2="476.663" gradientUnits="userSpaceOnUse"><stop stop-color="white"/><stop offset="1" stop-color="#999999" stop-opacity="0"/></linearGradient></defs></svg>
    """##
}

private struct SharedSVGIconView: View {
    let svg: String
    let fallbackSystemName: String
    let size: CGFloat
    var isTemplate = false

    #if os(iOS)
    @Environment(\.displayScale) private var displayScale
    #endif

    var body: some View {
        #if os(iOS)
        // One rasterized UIImage per unique (svg, size, scale), cached for the
        // app's lifetime. The previous WKWebView-per-icon approach paid a full
        // webview + async HTML paint per icon instance, so every drawer open
        // started with a field of blank squares popping in.
        if let image = SVGIconRasterizer.image(svg: svg, pointSize: size, scale: displayScale) {
            Image(uiImage: image)
                .renderingMode(isTemplate ? .template : .original)
                .frame(width: size, height: size)
                .allowsHitTesting(false)
        } else {
            fallback
        }
        #else
        fallback
        #endif
    }

    private var fallback: some View {
        Image(systemName: fallbackSystemName)
            .font(.system(size: size * 0.85, weight: .medium))
            .frame(width: size, height: size)
    }
}

#if os(iOS)
/// App-lifetime raster cache for the bundled icon SVGs. Rendering is
/// synchronous Core Graphics, so the first frame of a fresh drawer already
/// shows every icon — no async webview paint, no blank-then-filled pop-in.
@MainActor
enum SVGIconRasterizer {
    private struct Key: Hashable {
        let svg: String
        let pointSize: CGFloat
        let scale: CGFloat
    }

    private static var images: [Key: UIImage] = [:]
    /// Parse results are cached separately (including failures) so a broken
    /// SVG never re-parses per body evaluation.
    private static var parsedIcons: [String: ParsedSVGIcon?] = [:]

    static func image(svg: String, pointSize: CGFloat, scale: CGFloat) -> UIImage? {
        guard pointSize > 0 else { return nil }
        let key = Key(svg: svg, pointSize: pointSize, scale: scale)
        if let cached = images[key] { return cached }
        guard let icon = parsedIcon(for: svg) else { return nil }
        let target = CGSize(width: pointSize, height: pointSize)
        let format = UIGraphicsImageRendererFormat()
        format.scale = scale
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: target, format: format).image { rendererContext in
            icon.draw(in: rendererContext.cgContext, targetSize: target)
        }
        images[key] = image
        return image
    }

    private static func parsedIcon(for svg: String) -> ParsedSVGIcon? {
        if let cached = parsedIcons[svg] { return cached }
        let parsed = ParsedSVGIcon.parse(svg)
        parsedIcons[svg] = parsed
        return parsed
    }
}
#endif

// MARK: - Minimal SVG icon model

/// Parsed form of one of the app's own bundled icon SVGs (`UnpeelChromeIcon` /
/// `UnpeelToolIcon` in UnpeelShared): paths with solid or linear-gradient
/// fills and optional strokes. This is a purpose-built renderer for those
/// assets, not a general SVG engine — anything it cannot represent fails
/// parsing and the view falls back to the SF Symbol.
struct ParsedSVGIcon {
    struct GradientStop {
        var offset: CGFloat
        var color: CGColor
    }

    struct LinearGradientPaint {
        var start: CGPoint
        var end: CGPoint
        var stops: [GradientStop]
    }

    enum Paint {
        case none
        case color(CGColor)
        case gradient(String)
    }

    struct PathElement {
        var path: CGPath
        var fill: Paint
        var fillRule: CGPathFillRule
        var stroke: CGColor?
        var strokeWidth: CGFloat
        var lineCap: CGLineCap
        var lineJoin: CGLineJoin
    }

    var viewBox: CGRect
    var gradients: [String: LinearGradientPaint]
    var paths: [PathElement]

    static func parse(_ svg: String) -> ParsedSVGIcon? {
        let delegate = SVGIconXMLDelegate()
        let parser = XMLParser(data: Data(svg.utf8))
        parser.delegate = delegate
        guard parser.parse(),
              let viewBox = delegate.viewBox,
              viewBox.width > 0, viewBox.height > 0,
              !delegate.paths.isEmpty
        else { return nil }
        return ParsedSVGIcon(viewBox: viewBox, gradients: delegate.gradients, paths: delegate.paths)
    }

    /// Draws aspect-fit centered into `targetSize` (SVG's default
    /// `preserveAspectRatio="xMidYMid meet"`, matching the old webview CSS).
    func draw(in context: CGContext, targetSize: CGSize) {
        let scale = min(targetSize.width / viewBox.width, targetSize.height / viewBox.height)
        context.saveGState()
        context.translateBy(
            x: (targetSize.width - viewBox.width * scale) / 2,
            y: (targetSize.height - viewBox.height * scale) / 2
        )
        context.scaleBy(x: scale, y: scale)
        context.translateBy(x: -viewBox.minX, y: -viewBox.minY)
        for element in paths {
            switch element.fill {
            case .none:
                break
            case .color(let color):
                context.addPath(element.path)
                context.setFillColor(color)
                context.fillPath(using: element.fillRule)
            case .gradient(let id):
                if let gradient = gradients[id], let cgGradient = gradient.cgGradient {
                    context.saveGState()
                    context.addPath(element.path)
                    context.clip(using: element.fillRule)
                    context.drawLinearGradient(
                        cgGradient,
                        start: gradient.start,
                        end: gradient.end,
                        options: [.drawsBeforeStartLocation, .drawsAfterEndLocation]
                    )
                    context.restoreGState()
                }
            }
            if let stroke = element.stroke, element.strokeWidth > 0 {
                context.addPath(element.path)
                context.setStrokeColor(stroke)
                context.setLineWidth(element.strokeWidth)
                context.setLineCap(element.lineCap)
                context.setLineJoin(element.lineJoin)
                context.strokePath()
            }
        }
        context.restoreGState()
    }
}

private extension ParsedSVGIcon.LinearGradientPaint {
    var cgGradient: CGGradient? {
        CGGradient(
            colorsSpace: CGColorSpace(name: CGColorSpace.sRGB),
            colors: stops.map(\.color) as CFArray,
            locations: stops.map(\.offset)
        )
    }
}

// MARK: - SVG XML parsing

private final class SVGIconXMLDelegate: NSObject, XMLParserDelegate {
    var viewBox: CGRect?
    var gradients: [String: ParsedSVGIcon.LinearGradientPaint] = [:]
    var paths: [ParsedSVGIcon.PathElement] = []

    /// Presentation attributes on `<svg>` are inherited by paths that do not
    /// set their own (the bundled icons rely on this).
    private var rootAttributes: [String: String] = [:]
    private var currentGradientID: String?
    private var currentGradient: ParsedSVGIcon.LinearGradientPaint?

    func parser(
        _ parser: XMLParser,
        didStartElement elementName: String,
        namespaceURI: String?,
        qualifiedName: String?,
        attributes: [String: String] = [:]
    ) {
        switch elementName {
        case "svg":
            rootAttributes = attributes
            viewBox = Self.parseViewBox(attributes["viewBox"])
        case "linearGradient":
            currentGradientID = attributes["id"]
            currentGradient = ParsedSVGIcon.LinearGradientPaint(
                start: CGPoint(x: Self.number(attributes["x1"]) ?? 0, y: Self.number(attributes["y1"]) ?? 0),
                end: CGPoint(x: Self.number(attributes["x2"]) ?? 0, y: Self.number(attributes["y2"]) ?? 0),
                stops: []
            )
        case "stop":
            guard currentGradient != nil else { return }
            let opacity = Self.number(attributes["stop-opacity"]) ?? 1
            if let color = Self.color(attributes["stop-color"], opacity: opacity) {
                currentGradient?.stops.append(ParsedSVGIcon.GradientStop(
                    offset: Self.number(attributes["offset"]) ?? 0,
                    color: color
                ))
            }
        case "path":
            appendPath(attributes)
        default:
            break
        }
    }

    func parser(
        _ parser: XMLParser,
        didEndElement elementName: String,
        namespaceURI: String?,
        qualifiedName: String?
    ) {
        guard elementName == "linearGradient" else { return }
        if let id = currentGradientID, let gradient = currentGradient, !gradient.stops.isEmpty {
            gradients[id] = gradient
        }
        currentGradientID = nil
        currentGradient = nil
    }

    private func appendPath(_ attributes: [String: String]) {
        guard let data = attributes["d"], let path = SVGPathParser.path(from: data) else { return }
        func attribute(_ name: String) -> String? {
            attributes[name] ?? rootAttributes[name]
        }

        let fillValue = attribute("fill") ?? "#000000"
        let fill: ParsedSVGIcon.Paint
        if fillValue == "none" {
            fill = .none
        } else if fillValue.hasPrefix("url(#"), fillValue.hasSuffix(")") {
            fill = .gradient(String(fillValue.dropFirst("url(#".count).dropLast()))
        } else if let color = Self.color(fillValue, opacity: Self.number(attribute("fill-opacity")) ?? 1) {
            fill = .color(color)
        } else {
            fill = .none
        }
        let fillRule: CGPathFillRule =
            (attribute("fill-rule") ?? attribute("clip-rule")) == "evenodd" ? .evenOdd : .winding

        var stroke: CGColor?
        if let strokeValue = attribute("stroke"), strokeValue != "none" {
            stroke = Self.color(strokeValue, opacity: Self.number(attribute("stroke-opacity")) ?? 1)
        }

        paths.append(ParsedSVGIcon.PathElement(
            path: path,
            fill: fill,
            fillRule: fillRule,
            stroke: stroke,
            strokeWidth: Self.number(attribute("stroke-width")) ?? 1,
            lineCap: attribute("stroke-linecap") == "round" ? .round : .butt,
            lineJoin: attribute("stroke-linejoin") == "round" ? .round : .miter
        ))
    }

    private static func parseViewBox(_ value: String?) -> CGRect? {
        guard let value else { return nil }
        let parts = value
            .split(whereSeparator: { $0 == " " || $0 == "," })
            .compactMap { Double($0) }
        guard parts.count == 4 else { return nil }
        return CGRect(x: parts[0], y: parts[1], width: parts[2], height: parts[3])
    }

    private static func number(_ value: String?) -> CGFloat? {
        guard let value, let double = Double(value.trimmingCharacters(in: .whitespaces)) else { return nil }
        return CGFloat(double)
    }

    private static func color(_ value: String?, opacity: CGFloat) -> CGColor? {
        guard var hex = value?.trimmingCharacters(in: .whitespaces), hex.hasPrefix("#") else { return nil }
        hex.removeFirst()
        if hex.count == 3 {
            hex = hex.map { "\($0)\($0)" }.joined()
        }
        guard hex.count == 6, let rgb = UInt32(hex, radix: 16) else { return nil }
        return CGColor(
            srgbRed: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: opacity
        )
    }
}

// MARK: - SVG path data

enum SVGPathParser {
    /// Parses an SVG path `d` string into a CGPath. Supports the full
    /// command set the bundled icons use (M/L/H/V/C/S/Q/T/A/Z, absolute and
    /// relative, implicit repeats, packed arc flags). Returns nil on any
    /// syntax it does not understand.
    static func path(from data: String) -> CGPath? {
        var scanner = Scanner(data)
        let path = CGMutablePath()
        var current = CGPoint.zero
        var subpathStart = CGPoint.zero
        var lastCubicControl: CGPoint?
        var lastQuadControl: CGPoint?
        var command: Character = " "

        while true {
            scanner.skipSeparators()
            guard !scanner.isAtEnd else { break }
            if let letter = scanner.takeCommandLetter() {
                command = letter
            } else if command == "M" {
                command = "L"
            } else if command == "m" {
                command = "l"
            } else if command == " " || command == "Z" || command == "z" {
                return nil
            }
            let relative = command.isLowercase
            var cubicControl: CGPoint?
            var quadControl: CGPoint?

            switch command {
            case "M", "m":
                guard let point = scanner.takePoint(relativeTo: relative ? current : nil) else { return nil }
                current = point
                subpathStart = point
                path.move(to: point)
            case "L", "l":
                guard let point = scanner.takePoint(relativeTo: relative ? current : nil) else { return nil }
                current = point
                path.addLine(to: point)
            case "H", "h":
                guard let x = scanner.takeNumber() else { return nil }
                current = CGPoint(x: relative ? current.x + x : x, y: current.y)
                path.addLine(to: current)
            case "V", "v":
                guard let y = scanner.takeNumber() else { return nil }
                current = CGPoint(x: current.x, y: relative ? current.y + y : y)
                path.addLine(to: current)
            case "C", "c":
                let origin = relative ? current : nil
                guard let control1 = scanner.takePoint(relativeTo: origin),
                      let control2 = scanner.takePoint(relativeTo: origin),
                      let end = scanner.takePoint(relativeTo: origin)
                else { return nil }
                path.addCurve(to: end, control1: control1, control2: control2)
                cubicControl = control2
                current = end
            case "S", "s":
                let origin = relative ? current : nil
                let control1 = lastCubicControl.map { reflect($0, about: current) } ?? current
                guard let control2 = scanner.takePoint(relativeTo: origin),
                      let end = scanner.takePoint(relativeTo: origin)
                else { return nil }
                path.addCurve(to: end, control1: control1, control2: control2)
                cubicControl = control2
                current = end
            case "Q", "q":
                let origin = relative ? current : nil
                guard let control = scanner.takePoint(relativeTo: origin),
                      let end = scanner.takePoint(relativeTo: origin)
                else { return nil }
                path.addQuadCurve(to: end, control: control)
                quadControl = control
                current = end
            case "T", "t":
                guard let end = scanner.takePoint(relativeTo: relative ? current : nil) else { return nil }
                let control = lastQuadControl.map { reflect($0, about: current) } ?? current
                path.addQuadCurve(to: end, control: control)
                quadControl = control
                current = end
            case "A", "a":
                guard let rx = scanner.takeNumber(),
                      let ry = scanner.takeNumber(),
                      let rotation = scanner.takeNumber(),
                      let largeArc = scanner.takeFlag(),
                      let sweep = scanner.takeFlag(),
                      let end = scanner.takePoint(relativeTo: relative ? current : nil)
                else { return nil }
                appendArc(
                    to: path,
                    from: current,
                    to: end,
                    rx: rx,
                    ry: ry,
                    rotationDegrees: rotation,
                    largeArc: largeArc,
                    sweep: sweep
                )
                current = end
            case "Z", "z":
                path.closeSubpath()
                current = subpathStart
            default:
                return nil
            }
            lastCubicControl = cubicControl
            lastQuadControl = quadControl
        }
        return path.isEmpty ? nil : path
    }

    private static func reflect(_ point: CGPoint, about center: CGPoint) -> CGPoint {
        CGPoint(x: 2 * center.x - point.x, y: 2 * center.y - point.y)
    }

    /// SVG elliptical arc (endpoint parameterization, W3C F.6.5) approximated
    /// with cubic beziers in ≤90° segments.
    private static func appendArc(
        to path: CGMutablePath,
        from start: CGPoint,
        to end: CGPoint,
        rx: CGFloat,
        ry: CGFloat,
        rotationDegrees: CGFloat,
        largeArc: Bool,
        sweep: Bool
    ) {
        guard start != end else { return }
        var rx = abs(rx)
        var ry = abs(ry)
        guard rx > 0, ry > 0 else {
            path.addLine(to: end)
            return
        }
        let phi = rotationDegrees * .pi / 180
        let cosPhi = cos(phi)
        let sinPhi = sin(phi)

        let dx = (start.x - end.x) / 2
        let dy = (start.y - end.y) / 2
        let x1p = cosPhi * dx + sinPhi * dy
        let y1p = -sinPhi * dx + cosPhi * dy

        let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
        if lambda > 1 {
            let factor = sqrt(lambda)
            rx *= factor
            ry *= factor
        }

        let rx2 = rx * rx
        let ry2 = ry * ry
        let numerator = max(0, rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p)
        let denominator = rx2 * y1p * y1p + ry2 * x1p * x1p
        var coefficient = denominator == 0 ? 0 : sqrt(numerator / denominator)
        if largeArc == sweep { coefficient = -coefficient }

        let cxp = coefficient * (rx * y1p / ry)
        let cyp = coefficient * (-ry * x1p / rx)
        let center = CGPoint(
            x: cosPhi * cxp - sinPhi * cyp + (start.x + end.x) / 2,
            y: sinPhi * cxp + cosPhi * cyp + (start.y + end.y) / 2
        )

        func angle(_ ux: CGFloat, _ uy: CGFloat, _ vx: CGFloat, _ vy: CGFloat) -> CGFloat {
            let dot = ux * vx + uy * vy
            let length = sqrt((ux * ux + uy * uy) * (vx * vx + vy * vy))
            guard length > 0 else { return 0 }
            var value = acos(min(1, max(-1, dot / length)))
            if ux * vy - uy * vx < 0 { value = -value }
            return value
        }

        let startVectorX = (x1p - cxp) / rx
        let startVectorY = (y1p - cyp) / ry
        let theta1 = angle(1, 0, startVectorX, startVectorY)
        var deltaTheta = angle(startVectorX, startVectorY, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
        if !sweep, deltaTheta > 0 { deltaTheta -= 2 * .pi }
        if sweep, deltaTheta < 0 { deltaTheta += 2 * .pi }

        func ellipsePoint(_ theta: CGFloat) -> CGPoint {
            CGPoint(
                x: center.x + rx * cos(theta) * cosPhi - ry * sin(theta) * sinPhi,
                y: center.y + rx * cos(theta) * sinPhi + ry * sin(theta) * cosPhi
            )
        }

        func ellipseDerivative(_ theta: CGFloat) -> CGPoint {
            CGPoint(
                x: -rx * sin(theta) * cosPhi - ry * cos(theta) * sinPhi,
                y: -rx * sin(theta) * sinPhi + ry * cos(theta) * cosPhi
            )
        }

        let segments = max(1, Int(ceil(abs(deltaTheta) / (.pi / 2))))
        let step = deltaTheta / CGFloat(segments)
        let alpha = 4 / 3 * tan(step / 4)
        var theta = theta1
        for _ in 0..<segments {
            let thetaNext = theta + step
            let from = ellipsePoint(theta)
            let to = ellipsePoint(thetaNext)
            let derivativeFrom = ellipseDerivative(theta)
            let derivativeTo = ellipseDerivative(thetaNext)
            path.addCurve(
                to: to,
                control1: CGPoint(x: from.x + alpha * derivativeFrom.x, y: from.y + alpha * derivativeFrom.y),
                control2: CGPoint(x: to.x - alpha * derivativeTo.x, y: to.y - alpha * derivativeTo.y)
            )
            theta = thetaNext
        }
    }

    private struct Scanner {
        private let scalars: [UnicodeScalar]
        private var index = 0

        init(_ string: String) {
            scalars = Array(string.unicodeScalars)
        }

        var isAtEnd: Bool {
            index >= scalars.count
        }

        mutating func skipSeparators() {
            while index < scalars.count, Self.isSeparator(scalars[index]) {
                index += 1
            }
        }

        mutating func takeCommandLetter() -> Character? {
            skipSeparators()
            guard index < scalars.count else { return nil }
            let scalar = scalars[index]
            guard Self.commandLetters.contains(scalar) else { return nil }
            index += 1
            return Character(scalar)
        }

        mutating func takeNumber() -> CGFloat? {
            skipSeparators()
            let start = index
            var i = index
            var sawDigit = false
            var sawDot = false
            if i < scalars.count, scalars[i] == "-" || scalars[i] == "+" {
                i += 1
            }
            loop: while i < scalars.count {
                switch scalars[i] {
                case "0"..."9":
                    sawDigit = true
                    i += 1
                case ".":
                    if sawDot { break loop }
                    sawDot = true
                    i += 1
                default:
                    break loop
                }
            }
            guard sawDigit else { return nil }
            var text = ""
            text.unicodeScalars.append(contentsOf: scalars[start..<i])
            index = i
            guard let value = Double(text) else { return nil }
            return CGFloat(value)
        }

        /// Arc flags are single `0`/`1` characters and may be packed against
        /// the following number ("0 01-.104"), so they must not be parsed as
        /// ordinary numbers.
        mutating func takeFlag() -> Bool? {
            skipSeparators()
            guard index < scalars.count else { return nil }
            switch scalars[index] {
            case "0":
                index += 1
                return false
            case "1":
                index += 1
                return true
            default:
                return nil
            }
        }

        mutating func takePoint(relativeTo origin: CGPoint?) -> CGPoint? {
            guard let x = takeNumber(), let y = takeNumber() else { return nil }
            return CGPoint(x: (origin?.x ?? 0) + x, y: (origin?.y ?? 0) + y)
        }

        private static let commandLetters = Set("MmLlHhVvCcSsQqTtAaZz".unicodeScalars)

        private static func isSeparator(_ scalar: UnicodeScalar) -> Bool {
            scalar == " " || scalar == "," || scalar == "\n" || scalar == "\r" || scalar == "\t"
        }
    }
}
