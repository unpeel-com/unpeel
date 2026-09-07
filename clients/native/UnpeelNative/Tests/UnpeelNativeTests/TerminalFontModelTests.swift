import GhosttyTerminal
@testable import UnpeelNative
import XCTest

/// Settings ▸ Appearance ▸ Terminal font: persistence contract, the style
/// resolve, and the live overlay every retained pane receives.
@MainActor
final class TerminalFontModelTests: XCTestCase {
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUpWithError() throws {
        suiteName = "TerminalFontModelTests.\(UUID().uuidString)"
        defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
    }

    func testFreshSuiteResolvesShippedDefaults() {
        XCTAssertFalse(TerminalFontModel.hasSavedValues(in: defaults))
        let values = TerminalFontModel.savedValues(in: defaults)
        XCTAssertNil(values.family)
        XCTAssertEqual(values.size, TerminalFontModel.defaultSize)
    }

    func testWriteRoundTripsAndClampsToTheSettingsRange() {
        TerminalFontModel.write(family: "  Sarasa Mono K ", size: 99, to: defaults)
        XCTAssertTrue(TerminalFontModel.hasSavedValues(in: defaults))
        var values = TerminalFontModel.savedValues(in: defaults)
        XCTAssertEqual(values.family, "Sarasa Mono K")
        XCTAssertEqual(values.size, TerminalFontModel.sizeRange.upperBound)

        TerminalFontModel.write(family: nil, size: 2, to: defaults)
        values = TerminalFontModel.savedValues(in: defaults)
        XCTAssertNil(values.family)
        XCTAssertEqual(values.size, TerminalFontModel.sizeRange.lowerBound)

        // An explicit default is stored as "" so a workspace can override an
        // inherited custom family back to the shipped stack.
        XCTAssertEqual(defaults.string(forKey: "terminal_font_family"), "")

        TerminalFontModel.clearSavedValues(in: defaults)
        XCTAssertFalse(TerminalFontModel.hasSavedValues(in: defaults))
    }

    func testResolvedStyleFollowsTheThemeMirrors() {
        let previousFamily = Theme.terminalFontFamily
        let previousSize = Theme.terminalFontSize
        defer {
            Theme.terminalFontFamily = previousFamily
            Theme.terminalFontSize = previousSize
        }

        Theme.terminalFontFamily = "Menlo"
        Theme.terminalFontSize = 17
        var style = TerminalPaneStyle.resolved()
        XCTAssertEqual(style.fontFamily, "Menlo")
        XCTAssertEqual(style.fontSize, 17)

        // nil = the shipped stack, resolved the same way the picker labels it.
        Theme.terminalFontFamily = nil
        Theme.terminalFontSize = TerminalFontModel.defaultSize
        style = TerminalPaneStyle.resolved()
        XCTAssertEqual(style.fontFamily, TerminalFontModel.shippedFamily())
        XCTAssertEqual(style.fontSize, Float(TerminalFontModel.defaultSize))
    }

    /// The overlay must clear Ghostty's repeatable `font-family` list before
    /// naming the new face — otherwise the family set at construction stays
    /// primary and the change never shows.
    func testLiveOverlayClearsTheFamilyListBeforeNamingTheFont() {
        var style = TerminalPaneStyle.resolved()
        style.backgroundOpacity = 0.8
        style.fontSize = 15
        style.fontFamily = "Menlo"

        let expected = TerminalConfiguration { builder in
            builder.withBackgroundOpacity(0.8)
            builder.withFontSize(15)
            builder.withFontFamily("")
            builder.withFontFamily("Menlo")
        }
        XCTAssertEqual(GhosttyTerminalPane.surfaceOverlayConfiguration(for: style), expected)

        style.fontFamily = nil
        let cleared = TerminalConfiguration { builder in
            builder.withBackgroundOpacity(0.8)
            builder.withFontSize(15)
            builder.withFontFamily("")
        }
        XCTAssertEqual(GhosttyTerminalPane.surfaceOverlayConfiguration(for: style), cleared)
    }

    /// SurfaceCache pushes a retained pane's style only when its signature
    /// moves, so a font change has to be part of the signature.
    func testStyleSignatureMovesWithTheFont() {
        let base = TerminalPaneStyle.resolved()
        var larger = base
        larger.fontSize += 1
        var otherFace = base
        otherFace.fontFamily = "Menlo"

        let signature = { (style: TerminalPaneStyle) in
            SurfaceCache.styleSignature(background: nil, canvasSample: nil, paneStyle: style)
        }
        XCTAssertNotEqual(signature(base), signature(larger))
        XCTAssertNotEqual(signature(base), signature(otherFace))
        XCTAssertEqual(signature(base), signature(base))
    }

    func testInstalledFamiliesIncludeMonospacedSystemFacesOnly() {
        let families = TerminalFontModel.installedMonospacedFamilies()
        XCTAssertTrue(families.contains("Menlo"))
        XCTAssertFalse(families.contains("Helvetica"))
        XCTAssertFalse(families.contains { $0.hasPrefix(".") })
    }
}
