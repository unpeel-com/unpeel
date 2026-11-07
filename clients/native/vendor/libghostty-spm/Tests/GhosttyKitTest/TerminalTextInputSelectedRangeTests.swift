#if canImport(AppKit) && !canImport(UIKit)
    import Foundation
    @testable import GhosttyTerminal
    import Testing

    struct TerminalTextInputSelectedRangeTests {
        @Test
        func `idle selected range is a caret, never NSNotFound`() {
            let range = TerminalTextInputHandler.selectedRange(for: TerminalMarkedTextState())

            // macOS dictation refuses to start when the client reports
            // {NSNotFound, 0} as its selection.
            #expect(range.location != NSNotFound)
            #expect(range == NSRange(location: 0, length: 0))
        }

        @Test
        func `composing selected range is the caret inside the marked text`() {
            var state = TerminalMarkedTextState()
            state.setMarkedText("shufa", selectedRange: NSRange(location: 3, length: 0))

            #expect(
                TerminalTextInputHandler.selectedRange(for: state)
                    == NSRange(location: 3, length: 0)
            )
        }

        @Test
        func `selected range returns to a caret after composition clears`() {
            var state = TerminalMarkedTextState()
            state.setMarkedText("字", selectedRange: NSRange(location: 1, length: 0))
            state.clear()

            #expect(
                TerminalTextInputHandler.selectedRange(for: state)
                    == NSRange(location: 0, length: 0)
            )
        }
    }
#endif
