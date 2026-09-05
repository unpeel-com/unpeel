import XCTest
@testable import UnpeelNative

final class ThemeColorTests: XCTestCase {
    func testHostedAppAccentPaletteMatchesWorkspaceAndProjectColors() {
        XCTAssertNil(AppTint.none.accentHex)
        XCTAssertEqual(AppTint.peel.accentHex, "#D97757")
        XCTAssertEqual(AppTint.amber.accentHex, "#E3A63B")
        XCTAssertEqual(AppTint.green.accentHex, "#3FBF63")
        XCTAssertEqual(AppTint.teal.accentHex, "#4EC3C9")
        XCTAssertEqual(AppTint.blue.accentHex, "#4FA8FF")
        XCTAssertEqual(AppTint.indigo.accentHex, "#7A7EF2")
        XCTAssertEqual(AppTint.violet.accentHex, "#B166E8")

        XCTAssertEqual(ProjectFolderColor.sky.accentHex(isDark: false), "#2095C9")
        XCTAssertEqual(ProjectFolderColor.sky.accentHex(isDark: true), "#7DD3FC")
        XCTAssertEqual(ProjectFolderColor.teal.accentHex(isDark: false), "#159B91")
        XCTAssertEqual(ProjectFolderColor.teal.accentHex(isDark: true), "#64DCCB")
        XCTAssertEqual(ProjectFolderColor.graphite.accentHex(isDark: false), "#687083")
        XCTAssertEqual(ProjectFolderColor.graphite.accentHex(isDark: true), "#B8BCC8")
    }

    /// Dark hierarchy since a0f4f65 (2026-09-01): a near-black frame below a
    /// slightly lighter terminal surface.
    @MainActor
    func testDefaultDarkBackgroundIsDarkerThanSurface() {
        let previousHue = Theme.appTintHue
        let previousSurfaceTone = Theme.surfaceToneOverride
        defer {
            Theme.appTintHue = previousHue
            Theme.surfaceToneOverride = previousSurfaceTone
        }

        Theme.appTintHue = nil
        Theme.surfaceToneOverride = nil

        XCTAssertEqual(Theme.darkBackgroundHex, 0x121314)
        XCTAssertEqual(Theme.darkSurfaceHex, 0x1A1B1D)
        XCTAssertLessThan(
            TransparencyModel.designBackgroundTone,
            TransparencyModel.designSurfaceTone
        )
        XCTAssertEqual(
            TerminalPaneStyle.resolved().dark.background.lowercased(),
            Theme.darkSurfaceHexString.lowercased()
        )
    }

    @MainActor
    func testLegacyAutomaticTonePairsAdoptCurrentHierarchy() throws {
        let suiteName = "ThemeColorTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        for pair in [
            (0.12, 0.22),
            (0.12, 0.20),
            (0.12, 0.19),
            (0.12, 0.18),
            (0.12, 0.16),
        ] {
            TransparencyModel.write(
                background: TransparencyModel.backgroundMaterialOpacity,
                surface: 1,
                backgroundTone: pair.0,
                surfaceTone: pair.1,
                to: defaults
            )

            let values = TransparencyModel.savedValues(in: defaults)
            XCTAssertEqual(
                values.backgroundTone,
                TransparencyModel.designBackgroundTone,
                accuracy: 0.0001
            )
            XCTAssertEqual(
                values.surfaceTone,
                TransparencyModel.designSurfaceTone,
                accuracy: 0.0001
            )
        }
    }
}
