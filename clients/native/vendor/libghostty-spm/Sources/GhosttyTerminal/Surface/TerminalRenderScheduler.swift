import Foundation

/// One refresh for the pending batch, with no idle timer or display-link
/// subscription. Transport threads may request work; delivery is on main.
/// Requests coalesce until the next main-queue turn, without a trailing timer.
final class TerminalRenderScheduler: @unchecked Sendable {
    private let lock = NSLock()
    private var enabled = true
    private var pending: DispatchWorkItem?
    private var generation: UInt64 = 0
    private let enqueue: @Sendable (DispatchWorkItem) -> Void
    private let render: @MainActor @Sendable () -> Void

    init(
        enqueue: @escaping @Sendable (DispatchWorkItem) -> Void = {
            DispatchQueue.main.async(execute: $0)
        },
        render: @escaping @MainActor @Sendable () -> Void
    ) {
        self.enqueue = enqueue
        self.render = render
    }

    func request() {
        lock.lock()
        guard enabled, pending == nil else {
            lock.unlock()
            return
        }
        generation &+= 1
        let requestedGeneration = generation
        let work = DispatchWorkItem { [weak self] in
            self?.deliver(generation: requestedGeneration)
        }
        pending = work
        lock.unlock()
        enqueue(work)
    }

    func setEnabled(_ enabled: Bool) {
        lock.lock()
        self.enabled = enabled
        if !enabled { cancelLocked() }
        lock.unlock()
    }

    func cancel() {
        lock.lock()
        cancelLocked()
        lock.unlock()
    }

    private func cancelLocked() {
        generation &+= 1
        pending?.cancel()
        pending = nil
    }

    private func deliver(generation requestedGeneration: UInt64) {
        lock.lock()
        guard enabled, generation == requestedGeneration, pending != nil else {
            lock.unlock()
            return
        }
        pending = nil
        lock.unlock()
        // The default queue and test drivers deliver on main. No raw Ghostty
        // handle crosses threads, and teardown cancels before freeing it.
        MainActor.assumeIsolated { render() }
    }

    deinit { pending?.cancel() }
}
