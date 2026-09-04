import { spawnSync } from 'node:child_process'

function git(repoRoot, args, { optional = false, network = false } = {}) {
  const result = spawnSync('git', ['-C', repoRoot, ...args], {
    encoding: 'utf8',
    ...(network ? {
      timeout: 15_000,
      env: {
        ...process.env,
        GIT_TERMINAL_PROMPT: '0',
        GIT_SSH_COMMAND:
          process.env.GIT_SSH_COMMAND ?? 'ssh -o BatchMode=yes -o ConnectTimeout=10'
      }
    } : {})
  })
  if (result.status === 0) return result.stdout.trim()
  if (optional) return null
  const detail = result.error?.message
    || (result.stderr ?? '').trim()
    || (result.stdout ?? '').trim()
    || `exit ${result.status}`
  throw new Error(`git ${args.join(' ')} failed: ${detail}`)
}

export function readReleaseSourceState(repoRoot, { checkRemote = false } = {}) {
  const head = git(repoRoot, ['rev-parse', '--verify', 'HEAD'])
  const branch = git(repoRoot, ['symbolic-ref', '--quiet', '--short', 'HEAD'], { optional: true })
  const originMain = git(
    repoRoot,
    ['rev-parse', '--verify', 'refs/remotes/origin/main'],
    { optional: true }
  )
  const remoteMain = checkRemote
    ? git(repoRoot, ['ls-remote', '--exit-code', 'origin', 'refs/heads/main'], { network: true })
        .split(/\s+/)[0]
    : null
  const status = git(repoRoot, ['status', '--porcelain=v1', '--untracked-files=all'])
  return {
    head,
    branch,
    originMain,
    remoteMain,
    dirty: status.length > 0,
    dirtyEntries: status ? status.split('\n') : []
  }
}

export function assertPublishableReleaseSource(state) {
  if (state.branch !== 'main') {
    throw new Error(`real releases must run from branch main (current: ${state.branch ?? 'detached HEAD'})`)
  }
  if (state.dirty) {
    const sample = state.dirtyEntries.slice(0, 5).join(', ')
    throw new Error(`real releases require a clean worktree${sample ? ` (${sample})` : ''}`)
  }
  if (!state.originMain) {
    throw new Error('origin/main is unavailable; fetch origin before releasing')
  }
  if (state.head !== state.originMain) {
    throw new Error(
      `release HEAD ${state.head} does not match local origin/main ${state.originMain}; ` +
        'push/fetch main before releasing'
    )
  }
  if (state.remoteMain && state.head !== state.remoteMain) {
    throw new Error(
      `release HEAD ${state.head} does not match remote origin/main ${state.remoteMain}; ` +
        'push or update main before releasing'
    )
  }
}

export function cliBuildProvenance({ state, version, target }) {
  return {
    schema: 1,
    version,
    target,
    source_commit: state.head,
    source_dirty: state.dirty
  }
}

export function validateCliBuildProvenance({ provenance, version, target, sourceState, publishing }) {
  if (
    !provenance
    || typeof provenance !== 'object'
    || Array.isArray(provenance)
    || provenance.schema !== 1
    || provenance.version !== version
    || provenance.target !== target
    || !/^[0-9a-f]{40}$/.test(provenance.source_commit ?? '')
    || typeof provenance.source_dirty !== 'boolean'
  ) {
    throw new Error(`invalid BUILD_PROVENANCE.json for ${target}`)
  }
  if (provenance.source_commit !== sourceState.head) {
    throw new Error(
      `${target} was built from ${provenance.source_commit}, not current HEAD ${sourceState.head}`
    )
  }
  if (publishing && provenance.source_dirty) {
    throw new Error(`${target} was built from a dirty worktree`)
  }
}

export function validateCliBinaryTarget({ header, target, binary }) {
  if (!Buffer.isBuffer(header) || header.length < 20) {
    throw new Error(`${binary} in ${target} is too short to identify its architecture`)
  }
  if (target === 'linux-x86_64' || target === 'linux-aarch64') {
    if (
      header[0] !== 0x7f
      || header[1] !== 0x45
      || header[2] !== 0x4c
      || header[3] !== 0x46
      || header[4] !== 2
      || header[5] !== 1
    ) {
      throw new Error(`${binary} in ${target} is not a 64-bit little-endian ELF binary`)
    }
    const expectedMachine = target === 'linux-x86_64' ? 62 : 183
    const actualMachine = header.readUInt16LE(18)
    if (actualMachine !== expectedMachine) {
      throw new Error(
        `${binary} in ${target} has ELF machine ${actualMachine}, expected ${expectedMachine}`
      )
    }
    return
  }
  if (target === 'macos-universal') {
    const magic = header.readUInt32BE(0)
    const stride = magic === 0xcafebabe ? 20 : magic === 0xcafebabf ? 32 : null
    if (stride == null) {
      throw new Error(`${binary} in ${target} is not a universal Mach-O binary`)
    }
    const count = header.readUInt32BE(4)
    if (count === 0 || count > 32 || header.length < 8 + count * stride) {
      throw new Error(`${binary} in ${target} has an invalid universal Mach-O header`)
    }
    const cpuTypes = new Set()
    for (let index = 0; index < count; index += 1) {
      cpuTypes.add(header.readUInt32BE(8 + index * stride))
    }
    const x86_64 = 0x01000007
    const arm64 = 0x0100000c
    if (!cpuTypes.has(x86_64) || !cpuTypes.has(arm64)) {
      throw new Error(`${binary} in ${target} does not contain both arm64 and x86_64 slices`)
    }
    return
  }
  throw new Error(`unsupported CLI target: ${target}`)
}
