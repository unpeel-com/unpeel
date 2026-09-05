import Foundation

struct ProviderThemeReadRequest: Sendable {
    let sessionID: String
    let identity: UUID
    let sessionsDir: URL
    let command: String
    let workingDirectory: String?

    struct Result: Sendable {
        let background: TerminalFrameStyle.Background?
        let canvas: UInt32?
    }

    func read() -> Result {
        Result(
            background: TerminalFrameStyle.providerBackground(
                command: command, workingDirectory: workingDirectory
            ),
            canvas: ProviderCanvasSampler.dominantBackground(
                sessionID: sessionID, sessionsDir: sessionsDir
            )
        )
    }

    func matches(identity: UUID, sessionsDir: URL, command: String, workingDirectory: String?) -> Bool {
        self.identity == identity && self.sessionsDir == sessionsDir
            && self.command == command && self.workingDirectory == workingDirectory
    }
}
