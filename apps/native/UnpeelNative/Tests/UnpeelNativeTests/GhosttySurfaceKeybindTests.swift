@testable import UnpeelNative
import XCTest

@MainActor
final class GhosttySurfaceKeybindTests: XCTestCase {
    func testTerminalFontZoomBindingsSurviveDefaultKeybindClear() {
        let keybinds = Set(GhosttyTerminalPane.surfaceKeybinds)

        XCTAssertTrue(keybinds.contains("super+plus=increase_font_size:1"))
        XCTAssertTrue(keybinds.contains("super+==increase_font_size:1"))
        XCTAssertTrue(keybinds.contains("super+-=decrease_font_size:1"))
        XCTAssertTrue(keybinds.contains("super+zero=reset_font_size"))

        // "equal"/"minus" are PHYSICAL key names in Ghostty's bind parser and
        // physical matches beat codepoint matches. On layouts where the
        // dedicated "+" key sits on physical Minus (Norwegian, German, …) a
        // super+minus bind turns ⌘+ into zoom-out. Never re-add them.
        for keybind in keybinds {
            XCTAssertFalse(keybind.hasPrefix("super+equal="), keybind)
            XCTAssertFalse(keybind.hasPrefix("super+minus="), keybind)
        }
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
