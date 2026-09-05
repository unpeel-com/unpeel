//
//  WorktreeGit.swift
//  UnpeelNative
//
//  Git worktree plumbing for the native app. Worktrees live under
//  ~/.unpeel/worktrees/<repo-name>-<repo-hash>/<worktree-name-slug> so
//  multiple branches from the same repo stay grouped outside the checkout.
//
//  Everything here is blocking (Process + waitUntilExit); callers run it
//  off the main actor.
//

import Foundation

enum WorktreeGit {
    /// String-message result (Swift's Result needs Failure: Error, which a
    /// bare message string isn't).
    enum GitResult {
        case ok(String)
        case err(String)
    }

    // MARK: - git invocation (run_git, worktree.rs:24-47)

    /// Run git with `repo` as the working directory. Success → trimmed
    /// stdout; failure → trimmed stderr (or a generic message).
    static func runGit(repo: String, _ args: [String]) -> GitResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = args
        process.currentDirectoryURL = URL(fileURLWithPath: repo)
        let outputPipe = Pipe()
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = outputPipe
        process.standardError = outputPipe
        do {
            try process.run()
        } catch {
            return .err("Failed to run git: \(error.localizedDescription)")
        }
        // Drain before waiting so a chatty command can't fill stdout/stderr
        // and deadlock against waitUntilExit. Merging stderr into stdout keeps
        // one pipe to drain while still returning git's diagnostics on failure.
        let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let output = String(data: outputData, encoding: .utf8) ?? ""
        if process.terminationStatus == 0 {
            return .ok(output.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        let err = output.trimmingCharacters(in: .whitespacesAndNewlines)
        return .err(err.isEmpty ? "git \(args.joined(separator: " ")) failed" : err)
    }

    // MARK: - Path conventions (worktree.rs:58-93)

    /// Path slug: keep [A-Za-z0-9-_.], replace the rest with "-", trim
    /// leading/trailing "-"/".", empty -> "worktree".
    static func branchSlug(_ branch: String) -> String {
        var slug = ""
        for ch in branch {
            if ch.isASCII && (ch.isLetter || ch.isNumber) || ch == "-" || ch == "_" || ch == "." {
                slug.append(ch)
            } else {
                slug.append("-")
            }
        }
        let trimmed = slug.trimmingCharacters(in: CharacterSet(charactersIn: "-."))
        return trimmed.isEmpty ? "worktree" : trimmed
    }

    /// FNV-1a over the UTF-8 bytes, same constants as worktree.rs:75-82.
    static func fnv1aHash(_ input: String) -> UInt64 {
        var hash: UInt64 = 0xcbf29ce484222325
        for byte in input.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x100000001b3
        }
        return hash
    }

    /// `<repo-name-slug>-<hash:08x>` under the worktrees root — Rust's
    /// `{hash:08x}` zero-pads to at least 8 but prints all significant
    /// digits, so format the full value and pad only when short.
    static func repoWorktreesDir(worktreesRoot: URL, toplevel: String) -> URL {
        let repoName = branchSlug(URL(fileURLWithPath: toplevel).lastPathComponent)
        var hex = String(fnv1aHash(toplevel), radix: 16)
        if hex.count < 8 {
            hex = String(repeating: "0", count: 8 - hex.count) + hex
        }
        return worktreesRoot.appendingPathComponent("\(repoName)-\(hex)")
    }

    /// ~/.unpeel/worktrees, canonicalized like canonical_or_self —
    /// `git worktree list` reports resolved paths, so the dir we create
    /// must match what listings will say.
    static func worktreesRoot() -> URL {
        LaunchConfig.unpeelDir.appendingPathComponent("worktrees")
    }

    // MARK: - Branch listing (list_branches_impl, worktree.rs:256-272)

    /// Local branches, most recently committed first.
    static func listBranches(repoPath: String) -> [String] {
        guard case .ok(let out) = runGit(repo: repoPath, [
            "for-each-ref", "refs/heads", "--sort=-committerdate",
            "--format=%(refname:short)",
        ]) else { return [] }
        return out.split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    /// Remote-tracking branches (`origin/main`, …), most recently committed
    /// first, excluding the symbolic `<remote>/HEAD` entries.
    static func listRemoteBranches(repoPath: String) -> [String] {
        guard case .ok(let out) = runGit(repo: repoPath, [
            "for-each-ref", "refs/remotes", "--sort=-committerdate",
            "--format=%(refname:short)",
        ]) else { return [] }
        return out.split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty && !$0.hasSuffix("/HEAD") }
    }

    // MARK: - Default base ref

    /// The mainline ref new worktree branches fork from when the caller
    /// doesn't pick one: `origin/<default>` (via the `origin/HEAD` symref,
    /// falling back to `origin/main`/`origin/master`), then a local
    /// `main`/`master`. Nil means "no recognizable mainline" — fork from
    /// HEAD, the pre-2026-07 behavior.
    static func defaultBaseRef(repoPath: String) -> String? {
        if case .ok(let out) = runGit(repo: repoPath, [
            "symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD",
        ]), !out.isEmpty {
            return out
        }
        for candidate in ["origin/main", "origin/master"] {
            if case .ok = runGit(repo: repoPath, [
                "rev-parse", "--verify", "--quiet", "refs/remotes/\(candidate)",
            ]) {
                return candidate
            }
        }
        for candidate in ["main", "master"] {
            if case .ok = runGit(repo: repoPath, [
                "rev-parse", "--verify", "--quiet", "refs/heads/\(candidate)",
            ]) {
                return candidate
            }
        }
        return nil
    }

    /// Freshen a remote-tracking base ref (`<remote>/<branch>`) before
    /// forking from it. Strictly best-effort: never interactive
    /// (`GIT_TERMINAL_PROMPT=0`, no TTY), bounded by `timeout`, and failure
    /// is ignored — an offline or credential-prompting remote must never
    /// block session launch. Local refs are a no-op.
    static func bestEffortFetch(
        repoPath: String, baseRef: String, timeout: TimeInterval = 5
    ) {
        let parts = baseRef.split(separator: "/", maxSplits: 1)
        guard parts.count == 2 else { return }
        let remote = String(parts[0])
        let branch = String(parts[1])
        guard case .ok = runGit(
            repo: repoPath, ["remote", "get-url", remote]
        ) else { return }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["fetch", "--quiet", remote, branch]
        process.currentDirectoryURL = URL(fileURLWithPath: repoPath)
        var env = ProcessInfo.processInfo.environment
        env["GIT_TERMINAL_PROMPT"] = "0"
        process.environment = env
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let done = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in done.signal() }
        do {
            try process.run()
        } catch {
            return
        }
        if done.wait(timeout: .now() + timeout) == .timedOut {
            kill(process.processIdentifier, SIGKILL)
            done.wait()
        }
    }

    /// Branch checked out at `repoPath` (the base-picker fallback when the
    /// repo has no recognizable mainline); nil on a detached HEAD.
    static func currentBranch(repoPath: String) -> String? {
        guard case .ok(let out) = runGit(
            repo: repoPath, ["symbolic-ref", "--quiet", "--short", "HEAD"]
        ), !out.isEmpty else { return nil }
        return out
    }

    /// Branches already checked out in some worktree — git refuses to check
    /// them out a second time (mirrors usedBranches in NewWorkspaceView.svelte).
    static func checkedOutBranches(repoPath: String) -> Set<String> {
        guard case .ok(let out) = runGit(
            repo: repoPath, ["worktree", "list", "--porcelain"]
        ) else { return [] }
        var branches = Set<String>()
        for line in out.split(separator: "\n") where line.hasPrefix("branch ") {
            let ref = String(line.dropFirst("branch ".count))
            branches.insert(
                ref.hasPrefix("refs/heads/")
                    ? String(ref.dropFirst("refs/heads/".count)) : ref
            )
        }
        return branches
    }

    /// Path of the worktree that currently has `branch` checked out, if any.
    /// Includes the main checkout when its current branch matches.
    static func worktreePath(repoPath: String, branch: String) -> String? {
        let branch = branch.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !branch.isEmpty,
              case .ok(let out) = runGit(
                repo: repoPath, ["worktree", "list", "--porcelain"]
              )
        else { return nil }

        var currentPath: String?
        for line in out.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.hasPrefix("worktree ") {
                currentPath = String(line.dropFirst("worktree ".count))
                continue
            }
            if line.hasPrefix("branch ") {
                let ref = String(line.dropFirst("branch ".count))
                let checkedOut = ref.hasPrefix("refs/heads/")
                    ? String(ref.dropFirst("refs/heads/".count)) : ref
                if checkedOut == branch {
                    return currentPath.map {
                        URL(fileURLWithPath: $0).resolvingSymlinksInPath().path
                    }
                }
            }
            if line.isEmpty {
                currentPath = nil
            }
        }
        return nil
    }

    /// A checkout linked to a repository's main worktree. `branch` is the
    /// real local branch when present; detached Codex worktrees use a stable
    /// `detached@<commit>` marker so the existing Project contract can still
    /// represent them as worktree children.
    /// Provider-created worktrees (Claude, Codex, and other agents) are just
    /// ordinary Git worktrees, so discovery deliberately keys off Git's
    /// relationship instead of a provider-specific folder convention.
    struct LinkedWorktree: Hashable {
        let path: String
        let branch: String
    }

    /// Linked worktrees belonging to `repoPath`, excluding the main checkout.
    ///
    /// Returns nil when the path is not the repository's main checkout or Git
    /// cannot produce a trustworthy listing. That distinction lets callers
    /// avoid pruning previously discovered children on a transient failure.
    /// Detached checkouts receive a `detached@<commit>` marker; prunable or
    /// missing worktrees are skipped.
    static func linkedWorktrees(repoPath: String) -> [LinkedWorktree]? {
        guard case .ok(let out) = runGit(
            repo: repoPath, ["worktree", "list", "--porcelain", "-z"]
        ) else { return nil }

        struct Entry {
            var path: String?
            var head: String?
            var branch: String?
            var detached = false
            var prunable = false
        }

        var entries: [Entry] = []
        var current = Entry()
        func flush() {
            guard current.path != nil else { return }
            entries.append(current)
            current = Entry()
        }

        for rawField in out.split(separator: "\0", omittingEmptySubsequences: false) {
            let field = String(rawField)
            if field.isEmpty {
                flush()
            } else if field.hasPrefix("worktree ") {
                // Be tolerant of a malformed record without its separator.
                flush()
                current.path = String(field.dropFirst("worktree ".count))
            } else if field.hasPrefix("branch ") {
                let ref = String(field.dropFirst("branch ".count))
                current.branch = ref.hasPrefix("refs/heads/")
                    ? String(ref.dropFirst("refs/heads/".count)) : ref
            } else if field.hasPrefix("HEAD ") {
                current.head = String(field.dropFirst("HEAD ".count))
            } else if field == "detached" {
                current.detached = true
            } else if field == "prunable" || field.hasPrefix("prunable ") {
                current.prunable = true
            }
        }
        flush()

        guard let mainPath = entries.first?.path else { return nil }
        let canonicalRepo = URL(fileURLWithPath: repoPath).resolvingSymlinksInPath().path
        let canonicalMain = URL(fileURLWithPath: mainPath).resolvingSymlinksInPath().path
        // `git worktree list` always puts the main worktree first. Only the
        // main checkout may own sidebar children; adding a linked checkout as
        // a top-level project must not invert the tree and adopt its siblings.
        guard canonicalMain == canonicalRepo else { return nil }

        return entries.dropFirst().compactMap { entry in
            guard !entry.prunable, let path = entry.path,
                  FileManager.default.fileExists(atPath: path)
            else { return nil }
            let branch: String
            if let realBranch = entry.branch, !realBranch.isEmpty {
                branch = realBranch
            } else if entry.detached, let head = entry.head, !head.isEmpty {
                branch = "detached@\(head.prefix(12))"
            } else {
                return nil
            }
            return LinkedWorktree(
                path: URL(fileURLWithPath: path).resolvingSymlinksInPath().path,
                branch: branch
            )
        }
    }

    // MARK: - Create (create_worktree_impl, worktree.rs:173-215)

    enum CreateOutcome {
        case created(path: String)
        case failed(String)
    }

    /// Existing branch → check it out into a new worktree; unknown branch →
    /// create it from `baseRef` (`-b`). A nil/empty `baseRef` defaults to
    /// the repo's mainline (`defaultBaseRef`: `origin/<default>` after a
    /// best-effort fetch, else local main/master, else HEAD) — worktree
    /// sessions exist for parallel work that merges back to the mainline,
    /// so forking from a stale checkout's HEAD is the wrong silent default.
    /// Returns the worktree path. `folderName` is optional; when present it
    /// names the checkout folder independently of the git branch.
    static func createWorktree(
        repoPath: String, branch: String, baseRef: String? = nil, folderName: String? = nil
    ) -> CreateOutcome {
        let branch = branch.trimmingCharacters(in: .whitespacesAndNewlines)
        if branch.isEmpty {
            return .failed("Branch name is required")
        }
        if case .err = runGit(repo: repoPath, ["check-ref-format", "--branch", branch]) {
            return .failed("Invalid branch name: \(branch)")
        }
        let toplevel: String
        switch runGit(repo: repoPath, ["rev-parse", "--show-toplevel"]) {
        case .ok(let path): toplevel = path
        case .err(let error): return .failed(error)
        }

        let root = worktreesRoot()
        do {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        } catch {
            return .failed("Failed to create worktrees folder: \(error.localizedDescription)")
        }
        let canonicalRoot = root.resolvingSymlinksInPath()
        let trimmedFolderName = folderName?.trimmingCharacters(in: .whitespacesAndNewlines)
        let folderSource = trimmedFolderName?.isEmpty == false ? trimmedFolderName! : branch
        let worktreeDir = repoWorktreesDir(worktreesRoot: canonicalRoot, toplevel: toplevel)
            .appendingPathComponent(branchSlug(folderSource))
        if FileManager.default.fileExists(atPath: worktreeDir.path) {
            return .failed("Worktree folder already exists: \(worktreeDir.path)")
        }

        let branchExists: Bool
        if case .ok = runGit(
            repo: toplevel, ["rev-parse", "--verify", "refs/heads/\(branch)"]
        ) {
            branchExists = true
        } else {
            branchExists = false
        }

        var base = baseRef?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let picked = base, picked.hasPrefix("-") {
            return .failed("Base ref cannot start with '-'")
        }
        if !branchExists, base?.isEmpty != false {
            base = defaultBaseRef(repoPath: toplevel)
        }
        if !branchExists, let base, base.contains("/") {
            bestEffortFetch(repoPath: toplevel, baseRef: base)
        }
        // --no-track: a remote-tracking base would otherwise become the new
        // branch's upstream (branch.autoSetupMerge), leaving every worktree
        // branch "behind origin/main" in git status and pulling main on
        // `git pull`.
        var addArgs = ["worktree", "add", "--no-track", "-b", branch, worktreeDir.path]
        if let base, !base.isEmpty {
            addArgs.append(base)
        }
        let result = branchExists
            ? runGit(repo: toplevel, ["worktree", "add", worktreeDir.path, branch])
            : runGit(repo: toplevel, addArgs)
        if case .err(let error) = result {
            return .failed(error)
        }
        return .created(path: worktreeDir.resolvingSymlinksInPath().path)
    }

    // MARK: - Remove (remove_worktree_impl, worktree.rs:217-244)

    enum RemoveOutcome {
        case removed
        /// The folder/registration was already gone — treat as removed,
        /// like the alreadyGone tolerance in Sidebar.svelte:96-103.
        case alreadyGone
        /// Git refused because the tree has modified/untracked files (or is
        /// locked). The caller can confirm with the user and retry with
        /// `force: true`.
        case dirty(String)
        case failed(String)
    }

    /// `git worktree remove` from the MAIN checkout (git refuses to remove
    /// the worktree the command runs in). Without `force` a dirty tree
    /// refuses (surfaced as `.dirty` so the caller can ask the user);
    /// the branch always survives either way.
    static func removeWorktree(
        repoPath: String?, worktreePath: String, force: Bool = false
    ) -> RemoveOutcome {
        // Resolve the main checkout when the caller has no parent project:
        // <main>/.git is the common dir of every linked worktree.
        var repo = repoPath
        if repo == nil {
            if case .ok(let common) = runGit(
                repo: worktreePath,
                ["rev-parse", "--path-format=absolute", "--git-common-dir"]
            ) {
                repo = URL(fileURLWithPath: common).deletingLastPathComponent().path
            }
        }
        guard let repo else {
            return .failed("Could not locate the repository for this worktree")
        }
        if URL(fileURLWithPath: repo).resolvingSymlinksInPath().path
            == URL(fileURLWithPath: worktreePath).resolvingSymlinksInPath().path {
            return .failed("Cannot remove the main checkout")
        }
        var args = ["worktree", "remove"]
        if force {
            // Doubled on purpose: one --force removes a dirty tree, two are
            // required when the worktree is also locked.
            args += ["--force", "--force"]
        }
        args.append(worktreePath)
        switch runGit(repo: repo, args) {
        case .ok:
            return .removed
        case .err(let message):
            if message.contains("is not a working tree")
                || message.contains("No such file or directory") {
                return .alreadyGone
            }
            if message.contains("use --force") || message.contains("is locked") {
                return .dirty(message)
            }
            return .failed(message)
        }
    }
}
