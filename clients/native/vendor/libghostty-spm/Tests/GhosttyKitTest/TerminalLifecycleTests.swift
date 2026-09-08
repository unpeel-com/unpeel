@testable import GhosttyTerminal
import Testing

@MainActor
struct TerminalLifecycleTests {
    @Test
    func `failed surface creation does not retain bridge`() {
        let controller = TerminalController()
        let bridge = TerminalCallbackBridge()

        let surface = controller.createSurface(
            bridge: bridge,
            configuration: .init()
        ) { _ in }

        #expect(surface == nil)
        #expect(controller.retainedBridgeCount == 0)
    }

    @Test
    func `switching controllers removes bridge from old controller`() {
        let oldController = TerminalController()
        let newController = TerminalController()
        let coordinator = TerminalSurfaceCoordinator()

        coordinator.isAttached = { false }
        oldController.retain(coordinator.bridge)
        #expect(oldController.retainedBridgeCount == 1)

        coordinator.controller = oldController
        #expect(oldController.retainedBridgeCount == 0)

        oldController.retain(coordinator.bridge)
        #expect(oldController.retainedBridgeCount == 1)

        coordinator.controller = newController

        #expect(oldController.retainedBridgeCount == 0)
        #expect(newController.retainedBridgeCount == 0)
    }

    @Test
    func `removing controller detaches bridge from old controller`() {
        let controller = TerminalController()
        let coordinator = TerminalSurfaceCoordinator()

        coordinator.isAttached = { false }
        coordinator.controller = controller

        controller.retain(coordinator.bridge)
        #expect(controller.retainedBridgeCount == 1)

        // Surface-cache eviction reaches this path with `controller = nil`.
        // Logical teardown must finish synchronously even though physical
        // surface destruction is queued off the main actor.
        coordinator.controller = nil

        #expect(controller.retainedBridgeCount == 0)
    }

    @Test
    func `free surface removes retained bridge`() {
        let controller = TerminalController()
        let coordinator = TerminalSurfaceCoordinator()

        coordinator.isAttached = { false }
        coordinator.controller = controller

        controller.retain(coordinator.bridge)
        #expect(controller.retainedBridgeCount == 1)

        coordinator.freeSurface()

        #expect(controller.retainedBridgeCount == 0)
    }

    @Test
    func `hidden wakeup delivers callbacks without rendering`() {
        let controller = TerminalController()
        let coordinator = TerminalSurfaceCoordinator()
        var wakeups = 0
        var renders = 0

        coordinator.isAttached = { true }
        coordinator.setDisplayVisible(false)
        coordinator.onPostRender = { renders += 1 }
        controller.onWakeup = {
            wakeups += 1
            coordinator.requestImmediateTick()
        }

        controller.handleWakeup()
        coordinator.renderImmediately()

        #expect(wakeups == 1)
        #expect(renders == 0)
    }

    @Test
    func `hidden ancestor blocks rendering and adoption restores it`() {
        let coordinator = TerminalSurfaceCoordinator()
        var presented = false
        var renders = 0
        coordinator.isAttached = { true }
        coordinator.isPresented = { presented }
        coordinator.onPostRender = { renders += 1 }

        coordinator.refreshPresentationVisibility()
        coordinator.noteRenderActivity()
        coordinator.renderImmediately(synchronousDraw: true)
        #expect(renders == 0)

        presented = true
        coordinator.refreshPresentationVisibility()
        coordinator.renderImmediately(synchronousDraw: true)
        #expect(renders == 1)

        coordinator.isAttached = { false }
        coordinator.stopRendering()
        coordinator.renderImmediately()
        #expect(renders == 1)
    }

    @Test
    func `application active state controls immediate ticks`() async {
        let coordinator = TerminalSurfaceCoordinator()
        var renders = 0

        coordinator.isAttached = { true }
        coordinator.onPostRender = {
            renders += 1
        }

        coordinator.setApplicationActive(false)
        coordinator.requestImmediateTick()
        await Task.yield()

        #expect(renders == 0)

        coordinator.setApplicationActive(true)
        await Task.yield()

        #expect(renders == 1)
    }
}
