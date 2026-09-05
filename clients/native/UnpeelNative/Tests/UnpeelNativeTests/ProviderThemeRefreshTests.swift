import Foundation
import Testing
@testable import UnpeelNative

struct ProviderThemeRefreshTests {
    @Test
    func delayedThemeReadCannotUpdateAReplacementOrMovedPane() {
        let id = UUID()
        let home = URL(fileURLWithPath: "/tmp/theme-a")
        let request = ProviderThemeReadRequest(
            sessionID: "session", identity: id, sessionsDir: home,
            command: "opencode", workingDirectory: "/tmp/project"
        )
        #expect(request.matches(
            identity: id, sessionsDir: home, command: "opencode", workingDirectory: "/tmp/project"
        ))
        #expect(!request.matches(
            identity: UUID(), sessionsDir: home, command: "opencode", workingDirectory: "/tmp/project"
        ))
        #expect(!request.matches(
            identity: id, sessionsDir: URL(fileURLWithPath: "/tmp/theme-b"),
            command: "opencode", workingDirectory: "/tmp/project"
        ))
        #expect(!request.matches(
            identity: id, sessionsDir: home, command: "grok", workingDirectory: "/tmp/project"
        ))
        #expect(!request.matches(
            identity: id, sessionsDir: home, command: "opencode", workingDirectory: "/tmp/other"
        ))
    }

    @Test
    func sameSessionIDInDifferentHomesDoesNotReuseAnotherCanvas() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("up-theme-\(UUID().uuidString.prefix(8))")
        defer { try? FileManager.default.removeItem(at: root) }
        let homes = [root.appendingPathComponent("a"), root.appendingPathComponent("b")]
        let colors = [20, 40]
        for (home, color) in zip(homes, colors) {
            let session = home.appendingPathComponent("same-id")
            try FileManager.default.createDirectory(at: session, withIntermediateDirectories: true)
            let output = session.appendingPathComponent("output.bin")
            let bytes = Data(String(repeating: "\u{1b}[48;2;\(color);\(color);\(color)mX", count: 100).utf8)
            try bytes.write(to: output)
            try FileManager.default.setAttributes(
                [.modificationDate: Date(timeIntervalSince1970: 1_000)], ofItemAtPath: output.path
            )
        }
        #expect(ProviderCanvasSampler.dominantBackground(sessionID: "same-id", sessionsDir: homes[0]) == 0x141414)
        #expect(ProviderCanvasSampler.dominantBackground(sessionID: "same-id", sessionsDir: homes[1]) == 0x282828)
        #expect(ProviderCanvasSampler.dominantBackground(sessionID: "same-id", sessionsDir: homes[0]) == 0x141414)
    }
}
