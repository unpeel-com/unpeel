// dmg-background.swift — renders the drag-to-install DMG window background.
//
// Usage: swift dmg-background.swift <out-dir>
// Writes bg.png (1x) and bg@2x.png into <out-dir>; make-dmg.sh combines them
// into a HiDPI TIFF with tiffutil so the background stays crisp on retina.
//
// Geometry contract with make-dmg.sh's Finder layout:
//   window content area 520x360, Unpeel.app icon centered at (140, 165),
//   Applications alias centered at (380, 165) — the arrow is drawn between
//   those two points. Change them together.

import AppKit

let W: CGFloat = 520
let H: CGFloat = 360
let iconY: CGFloat = H - 165 // Finder positions are top-left-origin

func render(scale: CGFloat) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: W, height: H)

    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    defer { NSGraphicsContext.restoreGraphicsState() }

    let bounds = NSRect(x: 0, y: 0, width: W, height: H)

    // Near-black vertical gradient, matching the app's dark theme.
    let top = NSColor(calibratedRed: 0.105, green: 0.105, blue: 0.12, alpha: 1)
    let bottom = NSColor(calibratedRed: 0.06, green: 0.06, blue: 0.07, alpha: 1)
    NSGradient(colors: [top, bottom])!.draw(in: bounds, angle: -90)

    // Soft radial glow behind the icon row for depth.
    let glow = NSGradient(colors: [
        NSColor(calibratedWhite: 1, alpha: 0.045),
        NSColor(calibratedWhite: 1, alpha: 0),
    ])!
    glow.draw(
        fromCenter: NSPoint(x: W / 2, y: iconY), radius: 0,
        toCenter: NSPoint(x: W / 2, y: iconY), radius: 260,
        options: [])

    // Arrow between the two icons.
    let arrow = NSBezierPath()
    arrow.lineWidth = 9
    arrow.lineCapStyle = .round
    arrow.lineJoinStyle = .round
    let startX: CGFloat = 214
    let endX: CGFloat = 306
    arrow.move(to: NSPoint(x: startX, y: iconY))
    arrow.line(to: NSPoint(x: endX, y: iconY))
    arrow.move(to: NSPoint(x: endX - 20, y: iconY + 17))
    arrow.line(to: NSPoint(x: endX, y: iconY))
    arrow.line(to: NSPoint(x: endX - 20, y: iconY - 17))
    NSColor(calibratedWhite: 1, alpha: 0.28).setStroke()
    arrow.stroke()

    // Light plates behind the Finder icon labels. Finder always draws label
    // text in black when a background picture is set — even in dark mode
    // (longstanding limitation; no .DS_Store key controls it) — so the
    // artwork supplies a light surface under each label. Label geometry
    // measured empirically: 12pt labels center at y≈236 (top-origin) for
    // 112pt icons centered at y=165.
    // Frosted-glass style: translucent fill kept light (the text is black,
    // see above), brighter top gradient, hairline highlight, soft shadow.
    let labelCenterY = H - 238
    for (cx, w) in [(CGFloat(140), CGFloat(76)), (CGFloat(380), CGFloat(108))] {
        let rect = NSRect(x: cx - w / 2, y: labelCenterY - 12, width: w, height: 24)
        let pill = NSBezierPath(roundedRect: rect, xRadius: 12, yRadius: 12)

        NSGraphicsContext.current?.saveGraphicsState()
        let drop = NSShadow()
        drop.shadowColor = NSColor(calibratedWhite: 0, alpha: 0.4)
        drop.shadowOffset = NSSize(width: 0, height: -2)
        drop.shadowBlurRadius = 7
        drop.set()
        NSColor(calibratedWhite: 0.88, alpha: 0.8).setFill()
        pill.fill()
        NSGraphicsContext.current?.restoreGraphicsState()

        NSGradient(colors: [
            NSColor(calibratedWhite: 1, alpha: 0.55),
            NSColor(calibratedWhite: 1, alpha: 0.05),
        ])!.draw(in: pill, angle: -90)

        let border = NSBezierPath(
            roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: 11.5, yRadius: 11.5)
        border.lineWidth = 1
        NSColor(calibratedWhite: 1, alpha: 0.5).setStroke()
        border.stroke()
    }

    // Caption near the bottom.
    let caption = "Drag Unpeel to the Applications folder to install"
    let attrs: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 13, weight: .medium),
        .foregroundColor: NSColor(calibratedWhite: 1, alpha: 0.42),
    ]
    let size = caption.size(withAttributes: attrs)
    caption.draw(at: NSPoint(x: (W - size.width) / 2, y: 52), withAttributes: attrs)

    return rep
}

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: swift dmg-background.swift <out-dir>\n".data(using: .utf8)!)
    exit(2)
}
let outDir = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)

for (scale, name) in [(CGFloat(1), "bg.png"), (CGFloat(2), "bg@2x.png")] {
    let rep = render(scale: scale)
    guard let png = rep.representation(using: .png, properties: [:]) else {
        FileHandle.standardError.write("failed to encode \(name)\n".data(using: .utf8)!)
        exit(1)
    }
    try png.write(to: outDir.appendingPathComponent(name))
}
