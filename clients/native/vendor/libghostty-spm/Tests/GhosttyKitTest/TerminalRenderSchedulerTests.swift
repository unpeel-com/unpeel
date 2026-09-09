@testable import GhosttyTerminal
import Foundation
import Testing

@MainActor
struct TerminalRenderSchedulerTests {
    private final class Queue: @unchecked Sendable {
        private let lock = NSLock()
        private var items: [DispatchWorkItem] = []

        func enqueue(_ item: DispatchWorkItem) {
            lock.lock()
            items.append(item)
            lock.unlock()
        }

        func take() -> [DispatchWorkItem] {
            lock.lock()
            defer { lock.unlock() }
            let result = items
            items.removeAll()
            return result
        }
    }

    @Test
    func `burst produces one refresh and no idle tail`() {
        let queue = Queue()
        var renders = 0
        let scheduler = TerminalRenderScheduler(enqueue: { queue.enqueue($0) }) {
            renders += 1
        }
        for _ in 0..<100 { scheduler.request() }
        let batch = queue.take()
        #expect(batch.count == 1)
        batch.forEach { $0.perform() }
        #expect(renders == 1)
        #expect(queue.take().isEmpty)

        // A later low-rate write earns one more refresh, never a timed tail.
        scheduler.request()
        let next = queue.take()
        #expect(next.count == 1)
        next.forEach { $0.perform() }
        #expect(renders == 2)
        #expect(queue.take().isEmpty)
    }

    @Test
    func `occlusion cancels queued work and suppresses hidden host writes`() {
        let queue = Queue()
        var renders = 0
        let scheduler = TerminalRenderScheduler(enqueue: { queue.enqueue($0) }) {
            renders += 1
        }
        scheduler.request()
        let old = queue.take()
        scheduler.setEnabled(false)
        #expect(old.allSatisfy { $0.isCancelled })
        for _ in 0..<100 { scheduler.request() }
        #expect(queue.take().isEmpty)

        // Adoption schedules fresh work. A callback from before hiding must
        // neither paint nor consume the newly queued request.
        scheduler.setEnabled(true)
        scheduler.request()
        old.forEach { $0.perform() }
        #expect(renders == 0)
        let adopted = queue.take()
        #expect(adopted.count == 1)
        adopted.forEach { $0.perform() }
        #expect(renders == 1)
    }

    @Test
    func `concurrent host requests share one pending refresh`() {
        let queue = Queue()
        var renders = 0
        let scheduler = TerminalRenderScheduler(enqueue: { queue.enqueue($0) }) {
            renders += 1
        }
        DispatchQueue.concurrentPerform(iterations: 100) { _ in
            scheduler.request()
        }
        let batch = queue.take()
        #expect(batch.count == 1)
        batch.forEach { $0.perform() }
        #expect(renders == 1)
    }

    @Test
    func `request during rendering is delivered in the next batch`() {
        let queue = Queue()
        var renders = 0
        var scheduler: TerminalRenderScheduler!
        defer { scheduler = nil }
        scheduler = TerminalRenderScheduler(enqueue: { queue.enqueue($0) }) {
            renders += 1
            if renders == 1 { scheduler.request() }
        }
        scheduler.request()
        queue.take().forEach { $0.perform() }
        #expect(renders == 1)
        let next = queue.take()
        #expect(next.count == 1)
        next.forEach { $0.perform() }
        #expect(renders == 2)
        #expect(queue.take().isEmpty)
    }

    @Test
    func `hidden replay writes every byte without scheduling frames`() {
        let queue = Queue()
        var renders = 0
        var written = Data()
        let scheduler = TerminalRenderScheduler(enqueue: { queue.enqueue($0) }) {
            renders += 1
        }
        let session = InMemoryTerminalSession(write: { _ in }, resize: { _ in })
        session.writeBufferOverride = { written.append($0) }
        session.onHostBytes = { scheduler.request() }
        scheduler.setEnabled(false)
        session.receive("buffered replay")
        // The write override handles this fake pointer without calling Metal.
        session.setSurface(UnsafeMutableRawPointer(bitPattern: 1)!)
        session.receive(" plus live output")
        #expect(String(decoding: written, as: UTF8.self) == "buffered replay plus live output")
        #expect(queue.take().isEmpty)

        scheduler.setEnabled(true)
        scheduler.request()
        queue.take().forEach { $0.perform() }
        #expect(renders == 1)
        session.clearSurface(ifMatches: UnsafeMutableRawPointer(bitPattern: 1)!)
    }

    @Test
    func `synchronous draw cancels a redundant pending refresh`() {
        let queue = Queue()
        var renders = 0
        let scheduler = TerminalRenderScheduler(enqueue: { queue.enqueue($0) }) {
            renders += 1
        }
        scheduler.request()
        let pending = queue.take()
        scheduler.cancel()
        #expect(pending.allSatisfy { $0.isCancelled })
        pending.forEach { $0.perform() }
        #expect(renders == 0)
        scheduler.request()
        queue.take().forEach { $0.perform() }
        #expect(renders == 1)
    }
}
