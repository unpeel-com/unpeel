@testable import UnpeelNative
import XCTest

@MainActor
final class GhosttySurfaceKeybindTests: XCTestCase {
    /// Font zoom is app-owned since the Settings ▸ Appearance terminal font
    /// landed: the View menu's ⌘+ / ⌘= / ⌘− / ⌘0 edit `TerminalFontModel`
    /// (persisted, every pane follows). A surface-level bind would swallow
    /// the chord before NSMenu AND flip libghostty's `font_size_adjusted`,
    /// after which config reloads stop moving that surface's size — the
    /// Settings control would silently stop applying. Never re-add them.
    func testSurfaceBindsNoFontZoomSoTheViewMenuOwnsIt() {
        let keybinds = GhosttyTerminalPane.surfaceKeybinds

        for keybind in keybinds {
            XCTAssertFalse(keybind.contains("font_size"), keybind)
        }
        // Scrollback navigation still belongs to the surface.
        XCTAssertTrue(keybinds.contains("super+home=scroll_to_top"))
        XCTAssertTrue(keybinds.contains("super+end=scroll_to_bottom"))
    }

    /// ⌘V must stay `performable`: when the pasteboard has no text (e.g. a
    /// screenshot) the paste action is not performed and the key event
    /// falls through to the session, so kitty-protocol agents receive
    /// super+v and can paste the image themselves (orgs/unpeel-com
    /// discussions #11). A non-performable bind silently eats it.
    func testPasteBindingIsPerformableSoImagePasteReachesAgents() {
        let keybinds = Set(GhosttyTerminalPane.surfaceKeybinds)

        XCTAssertTrue(keybinds.contains("performable:super+v=paste_from_clipboard"))
        XCTAssertFalse(keybinds.contains("super+v=paste_from_clipboard"))
    }
}
