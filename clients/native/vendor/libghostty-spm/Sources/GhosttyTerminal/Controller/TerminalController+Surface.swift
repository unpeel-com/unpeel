//
//  TerminalController+Surface.swift
//  libghostty-spm
//

import Foundation
import GhosttyKit

extension TerminalController {
    /// Creates a new Ghostty surface with the given configuration.
    ///
    /// The `platformSetup` closure lets the caller fill in
    /// platform-specific fields (`platform_tag`, `platform`, `scale_factor`)
    /// on the raw surface config struct before the surface is created.
    func createSurface(
        bridge: TerminalCallbackBridge,
        configuration: TerminalSurfaceOptions,
        platformSetup: (inout ghostty_surface_config_s) -> Void
    ) -> TerminalSurface? {
        guard let app else { return nil }

        var surfaceConfig = ghostty_surface_config_new()
        let retainedBridge = Unmanaged.passRetained(bridge).toOpaque()
        surfaceConfig.userdata = retainedBridge
        surfaceConfig.context = configuration.context.ghosttyValue
        configureBackend(&surfaceConfig, from: configuration)

        if let fontSize = configuration.fontSize {
            surfaceConfig.font_size = fontSize
        }

        return finalizeSurface(
            app: app,
            bridge: bridge,
            configuration: configuration,
            config: &surfaceConfig,
            retainedBridge: retainedBridge,
            workingDirectory: configuration.workingDirectory,
            platformSetup: platformSetup
        )
    }

    func retain(_ bridge: TerminalCallbackBridge) {
        retainedBridges.append(bridge)
    }

    func remove(_ bridge: TerminalCallbackBridge) {
        retainedBridges.removeAll { $0 === bridge }
    }

    var retainedBridgeCount: Int {
        retainedBridges.count
    }

    private func configureBackend(
        _ config: inout ghostty_surface_config_s,
        from options: TerminalSurfaceOptions
    ) {
        guard case let .inMemory(session) = options.backend else {
            config.backend = GHOSTTY_SURFACE_IO_BACKEND_EXEC
            return
        }

        config.backend = GHOSTTY_SURFACE_IO_BACKEND_HOST_MANAGED
        config.receive_userdata = Unmanaged.passUnretained(session).toOpaque()
        config.receive_buffer = InMemoryTerminalSession.receiveBufferCallback
        config.receive_resize = InMemoryTerminalSession.receiveResizeCallback
    }

    private func finalizeSurface(
        app: ghostty_app_t,
        bridge: TerminalCallbackBridge,
        configuration: TerminalSurfaceOptions,
        config: inout ghostty_surface_config_s,
        retainedBridge: UnsafeMutableRawPointer,
        workingDirectory: String?,
        platformSetup: (inout ghostty_surface_config_s) -> Void
    ) -> TerminalSurface? {
        guard let workingDirectory else {
            return buildSurface(
                app: app,
                bridge: bridge,
                configuration: configuration,
                config: &config,
                retainedBridge: retainedBridge,
                platformSetup: platformSetup
            )
        }

        return workingDirectory.withCString { ptr in
            config.working_directory = ptr
            return buildSurface(
                app: app,
                bridge: bridge,
                configuration: configuration,
                config: &config,
                retainedBridge: retainedBridge,
                platformSetup: platformSetup
            )
        }
    }

    private func buildSurface(
        app: ghostty_app_t,
        bridge: TerminalCallbackBridge,
        configuration: TerminalSurfaceOptions,
        config: inout ghostty_surface_config_s,
        retainedBridge: UnsafeMutableRawPointer,
        platformSetup: (inout ghostty_surface_config_s) -> Void
    ) -> TerminalSurface? {
        platformSetup(&config)
        guard let surface = ghostty_surface_new(app, &config) else {
            releaseRetainedTerminalCallbackBridge(address: UInt(bitPattern: retainedBridge))
            return nil
        }

        retain(bridge)

        if case let .inMemory(session) = configuration.backend {
            session.setSurface(surface)
        }

        return TerminalSurface(surface, retainedUserdata: retainedBridge)
    }
}
