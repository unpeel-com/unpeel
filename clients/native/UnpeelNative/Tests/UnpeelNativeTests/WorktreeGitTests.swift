import XCTest
@testable import UnpeelNative

final class WorktreeGitTests: XCTestCase {
    func testRunGitDoesNotBlockWhenCommandWritesLargeStderr() throws {
        let repo = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-worktree-git-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: repo, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: repo) }

        guard case .ok = WorktreeGit.runGit(repo: repo.path, ["init"]) else {
            return XCTFail("expected git init to succeed")
        }

        let noisyAlias = #"!/bin/sh -c 'yes noisy-stderr | head -c 200000 >&2; exit 7'"#
        let result = WorktreeGit.runGit(
            repo: repo.path,
            ["-c", "alias.unpeel-spam=\(noisyAlias)", "unpeel-spam"]
        )

        guard case let .err(message) = result else {
            return XCTFail("expected noisy alias to fail")
        }
        XCTAssertTrue(message.contains("noisy-stderr"), message)
    }

    // MARK: - Default base ref

    private func makeTempDir() throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-worktree-git-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    @discardableResult
    private func git(_ repo: URL, _ args: [String]) -> String {
        let identity = [
            "-c", "user.email=test@unpeel.test", "-c", "user.name=Unpeel Test",
            "-c", "protocol.file.allow=always",
        ]
        guard case .ok(let out) = WorktreeGit.runGit(repo: repo.path, identity + args) else {
            XCTFail("git \(args.joined(separator: " ")) failed")
            return ""
        }
        return out
    }

    func testDefaultBaseRefPrefersOriginDefaultInClone() throws {
        let root = try makeTempDir()
        defer { try? FileManager.default.removeItem(at: root) }
        let origin = root.appendingPathComponent("origin")
        try FileManager.default.createDirectory(at: origin, withIntermediateDirectories: true)
        git(origin, ["-c", "init.defaultBranch=main", "init"])
        git(origin, ["commit", "--allow-empty", "-m", "c1"])
        let clone = root.appendingPathComponent("clone")
        git(root, ["clone", origin.path, clone.path])

        XCTAssertEqual(WorktreeGit.defaultBaseRef(repoPath: clone.path), "origin/main")

        // Sitting on a feature branch must not change the mainline default.
        git(clone, ["checkout", "-b", "feature/x"])
        XCTAssertEqual(WorktreeGit.defaultBaseRef(repoPath: clone.path), "origin/main")
    }

    func testDefaultBaseRefFallsBackToLocalMainWithoutRemote() throws {
        let repo = try makeTempDir()
        defer { try? FileManager.default.removeItem(at: repo) }
        git(repo, ["-c", "init.defaultBranch=main", "init"])
        git(repo, ["commit", "--allow-empty", "-m", "c1"])
        git(repo, ["checkout", "-b", "feature/x"])

        XCTAssertEqual(WorktreeGit.defaultBaseRef(repoPath: repo.path), "main")
    }

    func testDefaultBaseRefNilWithoutRecognizableMainline() throws {
        let repo = try makeTempDir()
        defer { try? FileManager.default.removeItem(at: repo) }
        git(repo, ["-c", "init.defaultBranch=trunk", "init"])
        git(repo, ["commit", "--allow-empty", "-m", "c1"])

        XCTAssertNil(WorktreeGit.defaultBaseRef(repoPath: repo.path))
    }

    func testBestEffortFetchFreshensRemoteTrackingRef() throws {
        let root = try makeTempDir()
        defer { try? FileManager.default.removeItem(at: root) }
        let origin = root.appendingPathComponent("origin")
        try FileManager.default.createDirectory(at: origin, withIntermediateDirectories: true)
        git(origin, ["-c", "init.defaultBranch=main", "init"])
        git(origin, ["commit", "--allow-empty", "-m", "c1"])
        let clone = root.appendingPathComponent("clone")
        git(root, ["clone", origin.path, clone.path])

        // origin/main moves after the clone; the clone's tracking ref is stale.
        git(origin, ["commit", "--allow-empty", "-m", "c2"])
        let tip = git(origin, ["rev-parse", "main"])
        XCTAssertNotEqual(git(clone, ["rev-parse", "origin/main"]), tip)

        WorktreeGit.bestEffortFetch(repoPath: clone.path, baseRef: "origin/main")
        XCTAssertEqual(git(clone, ["rev-parse", "origin/main"]), tip)

        // Local-looking refs and unknown remotes are harmless no-ops.
        WorktreeGit.bestEffortFetch(repoPath: clone.path, baseRef: "main")
        WorktreeGit.bestEffortFetch(repoPath: clone.path, baseRef: "feature/x")
    }

    // MARK: - External worktree discovery

    func testLinkedWorktreesFindsBranchAndDetachedChildrenOfMainCheckout() throws {
        let root = try makeTempDir()
        defer { try? FileManager.default.removeItem(at: root) }
        let repo = root.appendingPathComponent("repo")
        let claude = repo.appendingPathComponent(".claude/worktrees/chat-view")
        let detached = root.appendingPathComponent("detached")
        try FileManager.default.createDirectory(at: repo, withIntermediateDirectories: true)
        git(repo, ["-c", "init.defaultBranch=main", "init"])
        git(repo, ["commit", "--allow-empty", "-m", "c1"])
        git(repo, ["worktree", "add", "-b", "worktree-chat-view", claude.path])
        git(repo, ["worktree", "add", "--detach", detached.path])

        let detachedHead = git(detached, ["rev-parse", "HEAD"])
        XCTAssertEqual(Set(WorktreeGit.linkedWorktrees(repoPath: repo.path) ?? []), Set([
            .init(path: claude.resolvingSymlinksInPath().path, branch: "worktree-chat-view"),
            .init(
                path: detached.resolvingSymlinksInPath().path,
                branch: "detached@\(detachedHead.prefix(12))"
            ),
        ]))
        // A linked checkout cannot become the parent of the repository's
        // other worktrees merely because it was added as a top-level project.
        XCTAssertNil(WorktreeGit.linkedWorktrees(repoPath: claude.path))
    }
}
