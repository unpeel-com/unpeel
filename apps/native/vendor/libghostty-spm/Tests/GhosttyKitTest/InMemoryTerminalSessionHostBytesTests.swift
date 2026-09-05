@testable import GhosttyTerminal
import Foundation
import GhosttyKit
import Testing

/// Locks the render-arming contract of host-fed sessions: every byte fed
/// while a surface is attached must fire `onHostBytes` (which the surface
/// coordinator wires to its render pump), and an attach that flushes
/// buffered bytes must fire it too. This is the regression suite for the
/// "blank/stale regions until a manual resize" bug: bytes were written
/// into the surface but nothing armed the pump, so the core's coalesced
/// render wakeups let freshly parsed output sit on a stale frame.
struct InMemoryTerminalSessionHostBytesTests {
    /// Thread-safe event recorder (the session's callbacks are @Sendable).
    private final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private var storage: [String] = []

        func append(_ event: String) {
            lock.lock()
            storage.append(event)
            lock.unlock()
        }

        var events: [String] {
            lock.lock()
            defer { lock.unlock() }
            return storage
        }

        func reset() {
            lock.lock()
            storage.removeAll()
            lock.unlock()
        }
    }

    /// A nonnull pointer the session treats as an attached surface.
    /// Never dereferenced: `writeBufferOverride` replaces the raw write.
    private var fakeSurface: ghostty_surface_t {
        UnsafeMutableRawPointer(bitPattern: 0x1)!
    }

    private func makeSession(recording recorder: Recorder) -> InMemoryTerminalSession {
        let session = InMemoryTerminalSession(write: { _ in }, resize: { _ in })
        session.writeBufferOverride = { data in
            recorder.append("write:\(data.count)")
        }
        session.onHostBytes = { recorder.append("notify") }
        return session
    }

    @Test
    func `receive with surface writes then notifies`() {
        let recorder = Recorder()
        let session = makeSession(recording: recorder)
        session.setSurface(fakeSurface)
        recorder.reset() // attach itself notifies; covered separately

        session.receive(Data("hello".utf8))
        #expect(recorder.events == ["write:5", "notify"])

        session.receive(Data("ab".utf8))
        #expect(recorder.events == ["write:5", "notify", "write:2", "notify"])
    }

    @Test
    func `resize callback during host write does not deadlock`() {
        let recorder = Recorder()
        let session = InMemoryTerminalSession(
            write: { _ in },
            resize: { viewport in
                recorder.append("resize:\(viewport.columns)x\(viewport.rows)")
            }
        )
        session.writeBufferOverride = { [weak session] data in
            recorder.append("write:\(data.count)")
            guard let session else { return }
            InMemoryTerminalSession.receiveResizeCallback(
                Unmanaged.passUnretained(session).toOpaque(),
                80,
                24,
                640,
                480
            )
        }
        session.onHostBytes = { recorder.append("notify") }
        session.setSurface(fakeSurface)
        recorder.reset()

        let completed = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            session.receive(Data("host output".utf8))
            completed.signal()
        }

        #expect(completed.wait(timeout: .now() + 1) == .success)
        #expect(recorder.events == ["write:11", "resize:80x24", "notify"])
    }

    @Test
    func `receive without surface buffers and does not notify`() {
        let recorder = Recorder()
        let session = makeSession(recording: recorder)

        session.receive(Data("buffered".utf8))
        #expect(recorder.events.isEmpty)
    }

    @Test
    func `attach flushes buffered bytes and notifies`() {
        let recorder = Recorder()
        let session = makeSession(recording: recorder)

        session.receive(Data("abc".utf8))
        session.receive(Data("de".utf8))
        #expect(recorder.events.isEmpty)

        // Attach: the buffered chunks flush as one write, and the attach
        // notifies so the replayed content presents immediately (a cache
        // remount must not wait for the next live byte to paint).
        session.setSurface(fakeSurface)
        #expect(recorder.events == ["write:5", "notify"])
    }

    @Test
    func `attach with nothing buffered still notifies`() {
        let recorder = Recorder()
        let session = makeSession(recording: recorder)

        session.setSurface(fakeSurface)
        #expect(recorder.events == ["notify"])
    }

    @Test
    func `cleared handler stops notifications`() {
        let recorder = Recorder()
        let session = makeSession(recording: recorder)
        session.setSurface(fakeSurface)
        recorder.reset()

        session.onHostBytes = nil
        session.receive(Data("x".utf8))
        #expect(recorder.events == ["write:1"])
    }

    @Test
    func `detached surface stops write and notify`() {
        let recorder = Recorder()
        let session = makeSession(recording: recorder)
        session.setSurface(fakeSurface)
        recorder.reset()

        session.clearSurface(ifMatches: fakeSurface)
        session.receive(Data("x".utf8))
        #expect(recorder.events.isEmpty)
    }
}
