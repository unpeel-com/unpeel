//
//  TerminalSurface.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/16.
//

import Foundation
import GhosttyKit

enum TerminalResourceFreeQueue {
    private static let queue = DispatchQueue(
        label: "com.unpeel.ghostty-resource-free",
        qos: .utility
    )

    static func freeSurface(
        _ surface: ghostty_surface_t,
        afterFree: (@Sendable () -> Void)? = nil
    ) {
        let address = UInt(bitPattern: surface)
        queue.async {
            if let surface = UnsafeMutableRawPointer(bitPattern: address) {
                ghostty_surface_free(surface)
            }
            afterFree?()
        }
    }

    static func freeApp(
        _ app: ghostty_app_t,
        afterFree: (@Sendable () -> Void)? = nil
    ) {
        let address = UInt(bitPattern: app)
        queue.async {
            if let app = UnsafeMutableRawPointer(bitPattern: address) {
                ghostty_app_free(app)
            }
            afterFree?()
        }
    }

    static func freeConfig(_ config: ghostty_config_t) {
        let address = UInt(bitPattern: config)
        queue.async {
            guard let config = UnsafeMutableRawPointer(bitPattern: address) else { return }
            ghostty_config_free(config)
        }
    }
}

/// Thread-safe wrapper around `ghostty_surface_t`.
///
/// All access must happen on the main actor. The surface should be freed
/// explicitly via ``free()`` before the wrapper is deallocated; `deinit`
/// includes a safety net but relying on it is discouraged.
@MainActor
public final class TerminalSurface {
    private var surface: ghostty_surface_t?
    private var hasBeenFreed = false
    private let retainedUserdataAddress: UInt?

    init(_ surface: ghostty_surface_t, retainedUserdata: UnsafeMutableRawPointer? = nil) {
        self.surface = surface
        retainedUserdataAddress = retainedUserdata.map { UInt(bitPattern: $0) }
    }

    var rawValue: ghostty_surface_t? {
        surface
    }

    // MARK: - Input

    @discardableResult
    func sendKeyEvent(_ event: ghostty_input_key_s) -> Bool {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface key ignored: missing surface")
            return false
        }
        let result = ghostty_surface_key(s, event)
        TerminalDebugLog.log(
            .input,
            "surface key action=\(TerminalDebugLog.describe(event.action)) keycode=\(event.keycode) mods=0x\(String(event.mods.rawValue, radix: 16)) consumed=0x\(String(event.consumed_mods.rawValue, radix: 16)) text=\(terminalKeyText(event)) composing=\(event.composing) result=\(result)"
        )
        return result
    }

    /// Synthesize a full press+release of a key (by platform virtual
    /// keycode, e.g. `119` for End on macOS) with the given modifiers.
    /// Unlike `sendText`, the key goes through ghostty's key encoder and
    /// honors the keyboard protocol the running program negotiated (legacy
    /// vs kitty) — sending raw escape bytes as text mangles them for TUIs
    /// like Claude Code that enable the kitty protocol.
    /// `sendKeyPress` with the control modifier, for hosts that do not
    /// import the libghostty C module.
    @discardableResult
    public func sendControlKeyPress(keycode: UInt32) -> Bool {
        sendKeyPress(keycode: keycode, mods: GHOSTTY_MODS_CTRL)
    }

    @discardableResult
    public func sendKeyPress(keycode: UInt32, mods: ghostty_input_mods_e) -> Bool {
        var press = ghostty_input_key_s()
        press.action = GHOSTTY_ACTION_PRESS
        press.keycode = keycode
        press.mods = mods
        guard sendKeyEvent(press) else { return false }
        var release = ghostty_input_key_s()
        release.action = GHOSTTY_ACTION_RELEASE
        release.keycode = keycode
        release.mods = mods
        _ = sendKeyEvent(release)
        return true
    }

    @discardableResult
    public func sendText(_ text: String) -> Bool {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface text ignored: missing surface")
            return false
        }
        TerminalDebugLog.log(
            .input,
            "surface text=\(TerminalDebugLog.describe(text))"
        )
        text.withCString { cStr in
            ghostty_surface_text(s, cStr, UInt(text.utf8.count))
        }
        return true
    }

    @discardableResult
    func sendMouseButton(
        state: ghostty_input_mouse_state_e,
        button: ghostty_input_mouse_button_e,
        mods: ghostty_input_mods_e
    ) -> Bool {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface mouse button ignored: missing surface")
            return false
        }
        let result = ghostty_surface_mouse_button(s, state, button, mods)
        TerminalDebugLog.log(
            .input,
            "surface mouseButton state=\(TerminalDebugLog.describe(state)) button=\(button.rawValue) mods=0x\(String(mods.rawValue, radix: 16)) result=\(result)"
        )
        return result
    }

    func sendMousePos(x: Double, y: Double, mods: ghostty_input_mods_e) {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface mouse position ignored: missing surface")
            return
        }
        TerminalDebugLog.log(
            .input,
            "surface mousePos x=\(String(format: "%.2f", x)) y=\(String(format: "%.2f", y)) mods=0x\(String(mods.rawValue, radix: 16))"
        )
        ghostty_surface_mouse_pos(s, x, y, mods)
    }

    func sendMouseScroll(x: Double, y: Double, mods: ghostty_input_scroll_mods_t) {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface scroll ignored: missing surface")
            return
        }
        TerminalDebugLog.log(
            .input,
            "surface scroll x=\(String(format: "%.2f", x)) y=\(String(format: "%.2f", y)) mods=0x\(String(mods, radix: 16))"
        )
        ghostty_surface_mouse_scroll(s, x, y, mods)
    }

    func preedit(_ text: String) {
        guard let s = surface else {
            TerminalDebugLog.log(.ime, "surface preedit ignored: missing surface")
            return
        }
        TerminalDebugLog.log(.ime, "surface preedit=\(TerminalDebugLog.describe(text))")
        text.withCString { cStr in
            ghostty_surface_preedit(s, cStr, UInt(text.utf8.count))
        }
    }

    // MARK: - Actions

    /// Runs a Ghostty keybinding action by name (e.g. "scroll_to_bottom").
    @discardableResult
    public func performBindingAction(_ action: String) -> Bool {
        guard let s = surface else {
            TerminalDebugLog.log(.actions, "binding action ignored: missing surface")
            return false
        }
        let result = action.withCString { cStr in
            ghostty_surface_binding_action(s, cStr, UInt(action.utf8.count))
        }
        TerminalDebugLog.log(
            .actions,
            "binding action=\(TerminalDebugLog.describe(action)) result=\(result)"
        )
        return result
    }

    // MARK: - Rendering

    func draw() {
        guard let s = surface else { return }
        TerminalDebugLog.log(.render, "surface draw")
        ghostty_surface_draw(s)
    }

    func refresh() {
        guard let s = surface else { return }
        TerminalDebugLog.log(.render, "surface refresh")
        ghostty_surface_refresh(s)
    }

    func setSize(width: UInt32, height: UInt32) {
        guard let s = surface else {
            TerminalDebugLog.log(.metrics, "surface setSize ignored: missing surface")
            return
        }
        TerminalDebugLog.log(.metrics, "surface setSize \(width)x\(height)")
        ghostty_surface_set_size(s, width, height)
    }

    func setContentScale(x: Double, y: Double) {
        guard let s = surface else {
            TerminalDebugLog.log(.metrics, "surface contentScale ignored: missing surface")
            return
        }
        TerminalDebugLog.log(
            .metrics,
            "surface contentScale x=\(String(format: "%.2f", x)) y=\(String(format: "%.2f", y))"
        )
        ghostty_surface_set_content_scale(s, x, y)
    }

    // MARK: - State

    func setFocus(_ focused: Bool) {
        guard let s = surface else { return }
        TerminalDebugLog.log(.lifecycle, "surface focus=\(focused)")
        ghostty_surface_set_focus(s, focused)
    }

    func setColorScheme(_ scheme: ghostty_color_scheme_e) {
        guard let s = surface else { return }
        TerminalDebugLog.log(.lifecycle, "surface colorScheme=\(scheme.rawValue)")
        ghostty_surface_set_color_scheme(s, scheme)
    }

    func setOcclusion(_ visible: Bool) {
        guard let s = surface else { return }
        TerminalDebugLog.log(.lifecycle, "surface occlusion visible=\(visible)")
        ghostty_surface_set_occlusion(s, visible)
    }

    /// Bind the renderer's CVDisplayLink to the display actually hosting
    /// the surface. Without this the link created at renderer init runs
    /// against a default display — on a ProMotion or multi-display setup
    /// that caps vsync-driven draws at the wrong refresh rate (upstream
    /// Ghostty calls this from its SurfaceView on every screen change).
    func setDisplayID(_ displayID: UInt32) {
        guard let s = surface else { return }
        TerminalDebugLog.log(.lifecycle, "surface displayID=\(displayID)")
        ghostty_surface_set_display_id(s, displayID)
    }

    // MARK: - Size Query

    func size() -> TerminalGridMetrics? {
        guard let s = surface else {
            TerminalDebugLog.log(.metrics, "surface size query ignored: missing surface")
            return nil
        }
        let metrics = TerminalGridMetrics(ghostty_surface_size(s))
        TerminalDebugLog.log(.metrics, "surface size \(metrics.debugSummary)")
        return metrics
    }

    // MARK: - Read Viewport Row

    /// Returns the text of a single visible row (0-based from the top of the
    /// viewport), or `nil` if the row is out of range / no surface. Used by
    /// cmd-click file detection to scan the clicked line. Trailing cell padding
    /// is included by Ghostty; callers trim as needed.
    func readViewportRow(_ row: Int) -> String? {
        guard let s = surface else { return nil }
        let metrics = TerminalGridMetrics(ghostty_surface_size(s))
        guard metrics.columns > 0, row >= 0, row < Int(metrics.rows) else { return nil }

        let y = UInt32(row)
        let topLeft = ghostty_point_s(
            tag: GHOSTTY_POINT_VIEWPORT,
            coord: GHOSTTY_POINT_COORD_EXACT,
            x: 0,
            y: y
        )
        let bottomRight = ghostty_point_s(
            tag: GHOSTTY_POINT_VIEWPORT,
            coord: GHOSTTY_POINT_COORD_EXACT,
            x: UInt32(metrics.columns - 1),
            y: y
        )
        let selection = ghostty_selection_s(
            top_left: topLeft,
            bottom_right: bottomRight,
            rectangle: false
        )

        var out = ghostty_text_s()
        guard ghostty_surface_read_text(s, selection, &out) else { return nil }
        defer { ghostty_surface_free_text(s, &out) }

        guard let textPtr = out.text, out.text_len > 0 else { return "" }
        let bytes = UnsafeBufferPointer(start: textPtr, count: Int(out.text_len))
            .map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    /// Returns the full visible viewport as plain text (rows separated by
    /// newlines), or `nil` if there is no surface or the grid is empty. Used
    /// by hosts that scan the screen for TUI state markers (e.g. a
    /// full-screen TUI's "jump to bottom" hint). Trailing cell padding is
    /// included by Ghostty; callers trim/search as needed.
    public func readViewportText() -> String? {
        guard let s = surface else { return nil }
        let metrics = TerminalGridMetrics(ghostty_surface_size(s))
        guard metrics.columns > 0, metrics.rows > 0 else { return nil }

        let topLeft = ghostty_point_s(
            tag: GHOSTTY_POINT_VIEWPORT,
            coord: GHOSTTY_POINT_COORD_EXACT,
            x: 0,
            y: 0
        )
        let bottomRight = ghostty_point_s(
            tag: GHOSTTY_POINT_VIEWPORT,
            coord: GHOSTTY_POINT_COORD_EXACT,
            x: UInt32(metrics.columns - 1),
            y: UInt32(metrics.rows - 1)
        )
        let selection = ghostty_selection_s(
            top_left: topLeft,
            bottom_right: bottomRight,
            rectangle: false
        )

        var out = ghostty_text_s()
        guard ghostty_surface_read_text(s, selection, &out) else { return nil }
        defer { ghostty_surface_free_text(s, &out) }

        guard let textPtr = out.text, out.text_len > 0 else { return "" }
        let bytes = UnsafeBufferPointer(start: textPtr, count: Int(out.text_len))
            .map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    // MARK: - Selection

    struct SelectionResult {
        let text: String
        let offsetStart: UInt32
        let offsetLength: UInt32
    }

    func hasSelection() -> Bool {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface selection query ignored: missing surface")
            return false
        }
        let result = ghostty_surface_has_selection(s)
        TerminalDebugLog.log(.input, "surface hasSelection=\(result)")
        return result
    }

    func readSelection() -> String? {
        readSelectionResult()?.text
    }

    func readSelectionResult() -> SelectionResult? {
        guard let s = surface else {
            TerminalDebugLog.log(.input, "surface readSelection ignored: missing surface")
            return nil
        }
        var out = ghostty_text_s()
        guard ghostty_surface_read_selection(s, &out) else {
            TerminalDebugLog.log(.input, "surface readSelection returned false")
            return nil
        }
        defer { ghostty_surface_free_text(s, &out) }

        guard let textPtr = out.text, out.text_len > 0 else {
            TerminalDebugLog.log(.input, "surface readSelection empty")
            return SelectionResult(
                text: "",
                offsetStart: out.offset_start,
                offsetLength: out.offset_len
            )
        }

        let bytes = UnsafeBufferPointer(start: textPtr, count: Int(out.text_len))
            .map { UInt8(bitPattern: $0) }
        let text = String(decoding: bytes, as: UTF8.self)
        TerminalDebugLog.log(
            .input,
            "surface readSelection bytes=\(text.utf8.count) lines=\(TerminalInputText.lineCount(in: text)) offset=\(out.offset_start)+\(out.offset_len)"
        )
        return SelectionResult(
            text: text,
            offsetStart: out.offset_start,
            offsetLength: out.offset_len
        )
    }

    // MARK: - IME

    /// Caret cell rect in surface view points. Public so hosts can make
    /// smarter keyboard-avoidance decisions (skip shifting when the TUI's
    /// input line is already above the keyboard).
    public func imePoint() -> (x: Double, y: Double, width: Double, height: Double) {
        var x: Double = 0
        var y: Double = 0
        var w: Double = 0
        var h: Double = 0
        if let s = surface {
            ghostty_surface_ime_point(s, &x, &y, &w, &h)
        }
        TerminalDebugLog.log(
            .ime,
            "surface imePoint x=\(String(format: "%.2f", x)) y=\(String(format: "%.2f", y)) width=\(String(format: "%.2f", w)) height=\(String(format: "%.2f", h))"
        )
        return (x, y, w, h)
    }

    // MARK: - Mouse Capture

    var isMouseCaptured: Bool {
        guard let s = surface else { return false }
        return ghostty_surface_mouse_captured(s)
    }

    // MARK: - Quicklook Word (Apple-only)

    #if canImport(UIKit) || canImport(AppKit)
        struct QuicklookWordResult {
            let word: String
            let offsetStart: UInt32
            let offsetLength: UInt32
            // tl_px_x / tl_px_y are reported in host points (view coordinates),
            // not surface pixels. Ghostty's embedded API receives mouse_pos in
            // points and stores the cursor position * contentScale internally,
            // then divides by contentScale when reporting selection coordinates
            // back. Callers must convert cell pixel dimensions to points before
            // dividing.
            let pointX: Double
            let pointY: Double
        }

        func quicklookWord() -> QuicklookWordResult? {
            guard let s = surface else {
                TerminalDebugLog.log(.input, "surface quicklookWord ignored: missing surface")
                return nil
            }
            var out = ghostty_text_s()
            guard ghostty_surface_quicklook_word(s, &out) else {
                TerminalDebugLog.log(.input, "surface quicklookWord returned false")
                return nil
            }
            defer { ghostty_surface_free_text(s, &out) }

            let word: String
            if let textPtr = out.text, out.text_len > 0 {
                let bytes = UnsafeBufferPointer(start: textPtr, count: Int(out.text_len))
                    .map { UInt8(bitPattern: $0) }
                word = String(decoding: bytes, as: UTF8.self)
            } else {
                word = ""
            }
            TerminalDebugLog.log(
                .input,
                "surface quicklookWord word=\(TerminalDebugLog.describe(word)) offset=\(out.offset_start)+\(out.offset_len) pointX=\(String(format: "%.2f", out.tl_px_x)) pointY=\(String(format: "%.2f", out.tl_px_y))"
            )
            return QuicklookWordResult(
                word: word,
                offsetStart: out.offset_start,
                offsetLength: out.offset_len,
                pointX: out.tl_px_x,
                pointY: out.tl_px_y
            )
        }

        func selectionContainsQuicklookWord() -> Bool {
            guard let selected = readSelectionResult(),
                  let word = quicklookWord(),
                  !word.word.isEmpty,
                  word.offsetLength > 0
            else { return false }

            let selectionStart = UInt64(selected.offsetStart)
            let selectionEnd = selectionStart + UInt64(selected.offsetLength)
            let wordStart = UInt64(word.offsetStart)
            let wordEnd = wordStart + UInt64(word.offsetLength)
            let contains = wordStart >= selectionStart && wordEnd <= selectionEnd
            TerminalDebugLog.log(
                .input,
                "surface selectionContainsQuicklookWord=\(contains) selection=\(selected.offsetStart)+\(selected.offsetLength) word=\(word.offsetStart)+\(word.offsetLength)"
            )
            return contains
        }
    #endif

    // MARK: - Lifecycle

    func free() {
        guard let s = detachForFree() else { return }
        ghostty_surface_free(s)
        releaseRetainedTerminalCallbackBridge(address: retainedUserdataAddress)
    }

    func freeAsync() {
        guard let s = detachForFree() else { return }
        let retainedUserdataAddress = retainedUserdataAddress
        TerminalResourceFreeQueue.freeSurface(s) {
            releaseRetainedTerminalCallbackBridge(address: retainedUserdataAddress)
        }
    }

    private func detachForFree() -> ghostty_surface_t? {
        guard !hasBeenFreed, let s = surface else { return nil }
        TerminalDebugLog.log(.lifecycle, "surface free")
        hasBeenFreed = true
        surface = nil
        return s
    }

    deinit {
        // Surface should be freed explicitly via free() before deinit.
        // The deinit safety net is intentionally removed because
        // Swift 6 strict concurrency prevents accessing @MainActor
        // state from nonisolated deinit.
    }
}

private func terminalKeyText(_ event: ghostty_input_key_s) -> String {
    guard let text = event.text else { return "nil" }
    return TerminalDebugLog.describe(String(cString: text))
}
