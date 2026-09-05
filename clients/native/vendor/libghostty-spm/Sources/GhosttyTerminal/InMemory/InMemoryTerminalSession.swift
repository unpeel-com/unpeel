//
//  InMemoryTerminalSession.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/16.
//

import Foundation
import GhosttyKit

public final class InMemoryTerminalSession: @unchecked Sendable {
    /// Bytes received while no surface is attached are queued (instead of
    /// silently dropped) and flushed on attach, so hosts can start feeding
    /// before the surface exists. Bounded; oldest bytes drop on overflow.
    private static let maximumPendingBytes = 8 * 1024 * 1024

    /// Protects the surface pointer, pending host bytes, and render callback.
    /// Calls into Ghostty intentionally happen while this lock is held so the
    /// surface cannot be freed or replaced during a raw-pointer operation.
    private let lock = NSLock()
    /// Ghostty can synchronously wait for its IO thread while accepting host
    /// bytes. That IO thread may report a resize before the write returns, so
    /// resize bookkeeping must never contend for the surface lock or the two
    /// threads form a lock inversion.
    private let resizeLock = NSLock()
    private var surface: ghostty_surface_t?
    private var lastResize: InMemoryTerminalViewport?
    /// Guarded by `resizeLock`. True once the host has synced a real size
    /// into the currently attached surface (see armResizeDispatch).
    private var resizeDispatchArmed = true
    /// Guarded by `resizeLock`. The last pixel size the host synced into the
    /// surface (see armResizeDispatch) — the plausibility anchor.
    private var expectedSyncedPixels: (width: UInt32, height: UInt32)?
    private var pendingReceiveBuffer = Data()
    private let writeHandler: @Sendable (Data) -> Void
    private let resizeHandler: @Sendable (InMemoryTerminalViewport) -> Void

    public init(
        write: @escaping @Sendable (Data) -> Void,
        resize: @escaping @Sendable (InMemoryTerminalViewport) -> Void
    ) {
        writeHandler = write
        resizeHandler = resize
    }

    // MARK: - Surface Lifecycle

    func setSurface(_ surface: ghostty_surface_t?) {
        lock.lock()
        self.surface = surface
        if surface != nil {
            // A freshly attached surface announces its creation-time default
            // size (e.g. 768x578) on core's thread, and that event can be
            // delivered AFTER the host's first real size sync — overwriting
            // it. Resize dispatch stays disarmed until the coordinator
            // confirms a sync (`armResizeDispatch`), so pre-sync defaults
            // never reach the host.
            resizeLock.lock()
            resizeDispatchArmed = false
            resizeLock.unlock()
        }
        TerminalDebugLog.log(
            .lifecycle,
            "in-memory session surface=\(surface == nil ? "nil" : "set")"
        )
        var notify: (@Sendable () -> Void)?
        if surface != nil {
            flushPendingReceiveBufferLocked()
            // Arm the render pump for the attach itself: a freshly attached
            // (or re-attached, e.g. cache remount) surface must present its
            // replayed content without waiting for the next byte.
            notify = hostBytesHandler
        }
        lock.unlock()
        notify?()
    }

    func clearSurface(ifMatches expectedSurface: ghostty_surface_t?) {
        lock.lock()
        defer { lock.unlock() }

        guard surface == expectedSurface else {
            TerminalDebugLog.log(
                .lifecycle,
                "in-memory session clear skipped expected=\(expectedSurface == nil ? "nil" : "set") current=\(surface == nil ? "nil" : "set")"
            )
            return
        }

        surface = nil
        TerminalDebugLog.log(.lifecycle, "in-memory session surface=nil matched")
    }

    var currentSurface: ghostty_surface_t? {
        lock.lock()
        defer { lock.unlock() }
        return surface
    }

    // MARK: - Viewport Read

    /// Returns the active viewport as a UTF-8 string, or `nil` if no surface
    /// is attached. Lines are separated by `\n`. The `ghostty_text_s`
    /// lifecycle (allocate via `ghostty_surface_read_text`, free via
    /// `ghostty_surface_free_text`) is fully encapsulated — callers never
    /// touch the C buffer.
    ///
    /// Selection grammar: `(VIEWPORT, TOP_LEFT)` to `(VIEWPORT, BOTTOM_RIGHT)`
    /// with `rectangle: false` (linear flow). This reads exactly the visible
    /// rows and ignores scrollback. Empty viewports return an empty string.
    ///
    /// Thread-safe: acquires the same `NSLock` as `receive(_:)` and
    /// `setSurface(_:)`, preventing reads against a surface mid-replacement.
    public func readViewportText() -> String? {
        lock.lock()
        defer { lock.unlock() }
        guard let surface else { return nil }

        let topLeft = ghostty_point_s(
            tag: GHOSTTY_POINT_VIEWPORT,
            coord: GHOSTTY_POINT_COORD_TOP_LEFT,
            x: 0,
            y: 0
        )
        let bottomRight = ghostty_point_s(
            tag: GHOSTTY_POINT_VIEWPORT,
            coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT,
            x: 0,
            y: 0
        )
        let selection = ghostty_selection_s(
            top_left: topLeft,
            bottom_right: bottomRight,
            rectangle: false
        )

        var out = ghostty_text_s()
        guard ghostty_surface_read_text(surface, selection, &out) else {
            return nil
        }
        defer { ghostty_surface_free_text(surface, &out) }

        guard let textPtr = out.text, out.text_len > 0 else {
            return ""
        }
        let bytes = UnsafeBufferPointer(start: textPtr, count: Int(out.text_len))
            .map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    func updateViewport(_ size: TerminalGridMetrics) {
        TerminalDebugLog.log(.metrics, "in-memory viewport update \(size.debugSummary)")
        dispatchResize(InMemoryTerminalViewport(
            columns: size.columns,
            rows: size.rows,
            widthPixels: size.widthPixels,
            heightPixels: size.heightPixels,
            cellWidthPixels: size.cellWidthPixels,
            cellHeightPixels: size.cellHeightPixels
        ))
    }

    // MARK: - Receiving Data

    /// Fired after host bytes are written into the surface. The surface
    /// coordinator wires this to its render pump: the embedded core
    /// coalesces render wakeups under IO load, so a purely host-fed surface
    /// (no PTY, no local input) can hold freshly parsed terminal state on a
    /// stale frame — blank or partial regions until a resize forces a draw.
    /// Arming the pump on every host write makes remote-fed output present
    /// like local IO. Invoked outside the session lock.
    var onHostBytes: (@Sendable () -> Void)? {
        get {
            lock.lock()
            defer { lock.unlock() }
            return hostBytesHandler
        }
        set {
            lock.lock()
            defer { lock.unlock() }
            hostBytesHandler = newValue
        }
    }

    private var hostBytesHandler: (@Sendable () -> Void)?

    /// Test seam: replaces the raw `ghostty_surface_write_buffer` call so
    /// unit tests can observe receive/flush/notify ordering without a live
    /// ghostty surface (which needs Metal and can't init headless).
    var writeBufferOverride: ((Data) -> Void)?

    /// Feed data into the terminal from the host backend. While no surface
    /// is attached the bytes are buffered (bounded) and flushed on attach.
    public func receive(_ data: Data) {
        lock.lock()
        guard let surface else {
            TerminalDebugLog.log(
                .output,
                "terminal <- host buffered \(TerminalDebugLog.describe(data))"
            )
            pendingReceiveBuffer.append(data)
            if pendingReceiveBuffer.count > Self.maximumPendingBytes {
                pendingReceiveBuffer.removeFirst(
                    pendingReceiveBuffer.count - Self.maximumPendingBytes
                )
            }
            lock.unlock()
            return
        }

        TerminalDebugLog.log(
            .output,
            "terminal <- host \(TerminalDebugLog.describe(data))"
        )

        if let writeBufferOverride {
            writeBufferOverride(data)
        } else {
            Self.writeBuffer(data, to: surface)
        }
        let notify = hostBytesHandler
        lock.unlock()
        notify?()
    }

    /// Caller must hold `lock`.
    private func flushPendingReceiveBufferLocked() {
        guard let surface, !pendingReceiveBuffer.isEmpty else { return }
        let buffered = pendingReceiveBuffer
        pendingReceiveBuffer = Data()
        TerminalDebugLog.log(
            .output,
            "terminal <- host flushed \(buffered.count) buffered bytes"
        )
        if let writeBufferOverride {
            writeBufferOverride(buffered)
        } else {
            Self.writeBuffer(buffered, to: surface)
        }
    }

    private static func writeBuffer(_ data: Data, to surface: ghostty_surface_t) {
        data.withUnsafeBytes { buffer in
            guard let ptr = buffer.baseAddress?.assumingMemoryBound(to: UInt8.self) else {
                return
            }
            ghostty_surface_write_buffer(surface, ptr, UInt(buffer.count))
        }
    }

    /// Feed a UTF-8 string into the terminal from the host backend.
    public func receive(_ string: String) {
        guard let data = string.data(using: .utf8) else { return }
        receive(data)
    }

    /// Inject input bytes directly into the host-side consumer.
    ///
    /// This bypasses `ghostty_surface_key` translation and is intended for
    /// control sequences that the in-memory backend must interpret itself.
    public func sendInput(_ data: Data) {
        TerminalDebugLog.log(
            .input,
            "host <- direct input \(TerminalDebugLog.describe(data))"
        )
        writeHandler(data)
    }

    // MARK: - Process Exit

    /// Signal that the host-managed process has exited.
    public func finish(exitCode: UInt32, runtimeMilliseconds: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        guard let surface else {
            TerminalDebugLog.log(
                .lifecycle,
                "process exit ignored: missing surface exitCode=\(exitCode) runtimeMs=\(runtimeMilliseconds)"
            )
            return
        }

        TerminalDebugLog.log(
            .lifecycle,
            "process exit exitCode=\(exitCode) runtimeMs=\(runtimeMilliseconds)"
        )
        ghostty_surface_process_exit(surface, exitCode, runtimeMilliseconds)
    }

    // MARK: - C Callbacks

    static let receiveBufferCallback: ghostty_surface_receive_buffer_cb = { userdata, ptr, len in
        guard let userdata, let ptr else { return }
        let session = Unmanaged<InMemoryTerminalSession>
            .fromOpaque(userdata)
            .takeUnretainedValue()
        let data = Data(bytes: ptr, count: len)
        TerminalDebugLog.log(
            .input,
            "host <- terminal \(TerminalDebugLog.describe(data))"
        )
        session.writeHandler(data)
    }

    static let receiveResizeCallback: ghostty_surface_receive_resize_cb = { userdata, cols, rows, widthPx, heightPx in
        guard let userdata else { return }
        let session = Unmanaged<InMemoryTerminalSession>
            .fromOpaque(userdata)
            .takeUnretainedValue()
        TerminalDebugLog.log(
            .metrics,
            "receive resize cols=\(cols) rows=\(rows) pixels=\(widthPx)x\(heightPx)"
        )
        session.dispatchResize(InMemoryTerminalViewport(
            columns: cols,
            rows: rows,
            widthPixels: widthPx,
            heightPixels: heightPx
        ))
    }

    /// Called by the surface coordinator after a real size sync reached the
    /// attached surface: resize events from core are meaningful from now on.
    /// The synced pixel size also anchors a plausibility check — core's
    /// creation-time default announcement (~800x578) can be delivered on its
    /// own thread even after arming, and a grid wildly smaller than what the
    /// host just synced is that artifact, never a real layout.
    public func armResizeDispatch(
        syncedWidthPixels: UInt32,
        syncedHeightPixels: UInt32
    ) {
        resizeLock.lock()
        resizeDispatchArmed = true
        expectedSyncedPixels = (syncedWidthPixels, syncedHeightPixels)
        resizeLock.unlock()
    }

    private func dispatchResize(_ resize: InMemoryTerminalViewport) {
        resizeLock.lock()
        guard resizeDispatchArmed else {
            resizeLock.unlock()
            TerminalDebugLog.log(
                .metrics,
                "resize dropped (pre-sync) cols=\(resize.columns) rows=\(resize.rows) pixels=\(resize.widthPixels)x\(resize.heightPixels)"
            )
            return
        }
        if let expected = expectedSyncedPixels,
           resize.widthPixels > 0, resize.heightPixels > 0,
           Double(resize.widthPixels) < Double(expected.width) * 0.5
               || Double(resize.heightPixels) < Double(expected.height) * 0.5 {
            resizeLock.unlock()
            TerminalDebugLog.log(
                .metrics,
                "resize dropped (implausible vs synced \(expected.width)x\(expected.height)) cols=\(resize.columns) rows=\(resize.rows) pixels=\(resize.widthPixels)x\(resize.heightPixels)"
            )
            return
        }
        let mergedResize = mergedResize(resize)
        guard mergedResize != lastResize else {
            resizeLock.unlock()
            TerminalDebugLog.log(
                .metrics,
                "resize unchanged cols=\(mergedResize.columns) rows=\(mergedResize.rows) pixels=\(mergedResize.widthPixels)x\(mergedResize.heightPixels) cell=\(mergedResize.cellWidthPixels)x\(mergedResize.cellHeightPixels)"
            )
            return
        }
        lastResize = mergedResize
        resizeLock.unlock()

        TerminalDebugLog.log(
            .metrics,
            "resize dispatched cols=\(mergedResize.columns) rows=\(mergedResize.rows) pixels=\(mergedResize.widthPixels)x\(mergedResize.heightPixels) cell=\(mergedResize.cellWidthPixels)x\(mergedResize.cellHeightPixels)"
        )
        resizeHandler(mergedResize)
    }

    /// The last viewport dispatched to the resize handler, if any — hosts
    /// use it to re-derive a session's true grid without forcing a resize.
    public func lastReportedViewport() -> InMemoryTerminalViewport? {
        resizeLock.lock()
        defer { resizeLock.unlock() }
        return lastResize
    }

    private func mergedResize(_ resize: InMemoryTerminalViewport) -> InMemoryTerminalViewport {
        guard let lastResize else { return resize }

        return InMemoryTerminalViewport(
            columns: resize.columns,
            rows: resize.rows,
            widthPixels: resize.widthPixels == 0 ? lastResize.widthPixels : resize.widthPixels,
            heightPixels: resize.heightPixels == 0 ? lastResize.heightPixels : resize.heightPixels,
            cellWidthPixels: resize.cellWidthPixels == 0 ? lastResize.cellWidthPixels : resize.cellWidthPixels,
            cellHeightPixels: resize.cellHeightPixels == 0 ? lastResize.cellHeightPixels : resize.cellHeightPixels
        )
    }
}
