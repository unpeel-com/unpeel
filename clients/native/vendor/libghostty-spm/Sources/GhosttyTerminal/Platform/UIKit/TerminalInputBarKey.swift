//
//  TerminalInputBarKey.swift
//  libghostty-spm
//

#if canImport(UIKit) && !targetEnvironment(macCatalyst)
    public enum TerminalInputAccessoryItem: Equatable, Sendable {
        case esc
        case ctrl
        case alt
        case command
        case tab
        case arrowLeft
        case arrowUp
        case arrowDown
        case arrowRight
        case symbol(String)
        case paste
        /// Backspace/delete. Supports press-and-hold key repeat, owned by the
        /// accessory bar — hosts that hide the system keyboard's dictation mic
        /// (secure text entry) lose its native backspace auto-repeat, so this
        /// key restores hold-to-delete.
        case backspace
        /// Resigns first responder (hides the software keyboard). Hosts that
        /// mirror focus in their own state should observe `onFocusChange`.
        case hideKeyboard
        /// App-defined action button (attach image, voice input, …): taps
        /// call `UITerminalView.onAccessoryHostAction` with the id. The bar
        /// stays dumb; the host owns the behavior.
        case hostAction(id: String, systemImage: String, label: String)
        case divider

        public static let defaultItems: [TerminalInputAccessoryItem] = [
            .esc,
            .tab,
            .ctrl,
            .alt,
            .command,
            .divider,
            .arrowLeft,
            .arrowUp,
            .arrowDown,
            .arrowRight,
            .divider,
            .symbol("|"),
            .symbol("/"),
            .symbol("~"),
            .symbol("-"),
            .symbol("_"),
            .symbol("`"),
            .symbol("'"),
            .symbol("\""),
            .paste,
        ]
    }

    enum TerminalInputBarKey {
        case esc
        case tab
        case arrowLeft
        case arrowUp
        case arrowDown
        case arrowRight
        case symbol(String)
        case paste
        case backspace
    }
#endif
