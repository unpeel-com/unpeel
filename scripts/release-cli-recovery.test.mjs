import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import {
  chmodSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync
} from 'node:fs'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import test from 'node:test'

const repoRoot = resolve(import.meta.dirname, '..')
const publisher = resolve(repoRoot, 'scripts/release-cli.mjs')
const workspaceVersion = readFileSync(resolve(repoRoot, 'crates/Cargo.toml'), 'utf8')
  .match(/^version = "([^"]+)"$/m)?.[1]
assert.ok(workspaceVersion, 'crates workspace version is present')
const head = spawnSync('git', ['-C', repoRoot, 'rev-parse', '--verify', 'HEAD'], {
  encoding: 'utf8'
}).stdout.trim()
const revision = head.slice(0, 12)
const targets = ['macos-universal', 'linux-x86_64', 'linux-aarch64']

function binaryHeader(target) {
  const header = Buffer.alloc(64)
  if (target === 'macos-universal') {
    header.writeUInt32BE(0xcafebabe, 0)
    header.writeUInt32BE(2, 4)
    header.writeUInt32BE(0x01000007, 8)
    header.writeUInt32BE(0x0100000c, 28)
  } else {
    header.set([0x7f, 0x45, 0x4c, 0x46, 2, 1])
    header.writeUInt16LE(target === 'linux-x86_64' ? 62 : 183, 18)
  }
  return header
}

function fixture({ withProtocol = true } = {}) {
  const root = mkdtempSync(resolve(tmpdir(), 'unpeel-cli-recovery-test-'))
  const archives = {}
  for (const target of targets) {
    const stage = resolve(root, `stage-${target}`)
    mkdirSync(stage)
    if (withProtocol) {
      // The real publisher copies the whole protocol/ directory; the fixture
      // carries the two files the required-entry check names.
      mkdirSync(resolve(stage, 'protocol'))
      for (const name of ['host-capabilities-v1.json', 'host-conformance-v1.json']) {
        writeFileSync(resolve(stage, 'protocol', name), '{}\n')
      }
      mkdirSync(resolve(stage, 'generated'))
      writeFileSync(resolve(stage, 'generated', 'GeneratedRuntimeCatalog.swift'), '// fixture\n')
    }
    for (const binary of ['unpeel', 'unpeel-host', 'unpeel-attach']) {
      const path = resolve(stage, binary)
      writeFileSync(path, binaryHeader(target))
      chmodSync(path, 0o755)
    }
    writeFileSync(resolve(stage, 'LICENSE'), 'fixture license\n')
    writeFileSync(resolve(stage, 'THIRD_PARTY_NOTICES.txt'), 'fixture notices\n')
    writeFileSync(resolve(stage, 'BUILD_PROVENANCE.json'), `${JSON.stringify({
      schema: 1,
      version: workspaceVersion,
      target,
      source_commit: head,
      source_dirty: false
    })}\n`)
    const archive = resolve(root, `unpeel-${workspaceVersion}-${target}.tar.gz`)
    const tar = spawnSync('tar', [
      '-czf', archive, '-C', stage,
      'unpeel', 'unpeel-host', 'unpeel-attach', 'LICENSE', 'THIRD_PARTY_NOTICES.txt', 'BUILD_PROVENANCE.json',
      ...(withProtocol ? ['protocol', 'generated'] : [])
    ], { encoding: 'utf8' })
    assert.equal(tar.status, 0, tar.stderr)
    archives[target] = archive
  }
  return { root, archives }
}

function runPublisher(state, extraArgs = []) {
  return spawnSync(process.execPath, [
    publisher,
    '--channel', 'beta',
    '--version', workspaceVersion,
    '--skip-build',
    '--macos-universal', state.archives['macos-universal'],
    '--linux-x86_64', state.archives['linux-x86_64'],
    '--linux-aarch64', state.archives['linux-aarch64'],
    '--dry-run',
    ...extraArgs
  ], {
    cwd: repoRoot,
    encoding: 'utf8',
    env: { ...process.env, TMPDIR: state.root }
  })
}

function uploadKeys(stdout) {
  return [...stdout.matchAll(/"unpeel-releases\/([^"\n]+)"/g)].map((match) => match[1])
}

test('revision recovery uploads every immutable object before aliases and manifest', () => {
  const state = fixture()
  try {
    const result = runPublisher(state, ['--artifact-revision', revision])
    assert.equal(result.status, 0, result.stderr)
    const immutable = targets.flatMap((target) => {
      const key = `beta/cli/unpeel-${workspaceVersion}-${revision}-${target}.tar.gz`
      return [key, `${key}.sha256`]
    })
    const mutable = targets.flatMap((target) => {
      const key = `beta/cli/unpeel-latest-${target}.tar.gz`
      return [key, `${key}.sha256`]
    })
    assert.deepEqual(uploadKeys(result.stdout), [
      ...immutable,
      ...mutable,
      'beta/cli/latest.json',
      // Published per channel after the manifest: the Worker serves these
      // from R2 and only falls back to its bundled copy when absent.
      'beta/cli/install.sh',
      'beta/cli/install-app.sh',
      'beta/protocol/app-registry.json'
    ])

    const publishDir = resolve(
      state.root,
      readdirSync(state.root).find((entry) => entry.startsWith('unpeel-cli-publish-'))
    )
    const latest = JSON.parse(readFileSync(resolve(publishDir, 'latest.json'), 'utf8'))
    assert.equal(latest.artifact_revision, revision)
    for (const target of targets) {
      const archiveName = `unpeel-${workspaceVersion}-${revision}-${target}.tar.gz`
      assert.equal(latest.targets[target].key, `beta/cli/${archiveName}`)
      assert.equal(
        readFileSync(resolve(publishDir, `${target}-versioned.sha256`), 'utf8').trim().split(/\s+/)[1],
        archiveName
      )
      assert.equal(
        readFileSync(resolve(publishDir, `${target}-latest.sha256`), 'utf8').trim().split(/\s+/)[1],
        `unpeel-latest-${target}.tar.gz`
      )
    }
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

test('normal publishing keeps the legacy immutable keys and manifest shape', () => {
  const state = fixture()
  try {
    const result = runPublisher(state)
    assert.equal(result.status, 0, result.stderr)
    const keys = uploadKeys(result.stdout)
    for (const target of targets) {
      assert.equal(keys.includes(`beta/cli/unpeel-${workspaceVersion}-${target}.tar.gz`), true)
      // The plain versioned key now carries an immutable sidecar too (the Mac
      // app build verifies the versioned archive against it).
      assert.equal(keys.includes(`beta/cli/unpeel-${workspaceVersion}-${target}.tar.gz.sha256`), true)
    }
    const publishDir = resolve(
      state.root,
      readdirSync(state.root).find((entry) => entry.startsWith('unpeel-cli-publish-'))
    )
    const latest = JSON.parse(readFileSync(resolve(publishDir, 'latest.json'), 'utf8'))
    assert.equal(Object.hasOwn(latest, 'artifact_revision'), false)
    for (const target of targets) {
      assert.equal(latest.targets[target].key, `beta/cli/unpeel-${workspaceVersion}-${target}.tar.gz`)
      assert.equal(Object.hasOwn(latest.targets[target], 'sidecar_key'), false)
    }
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

test('an archive without protocol/ is refused before any upload is planned', () => {
  const state = fixture({ withProtocol: false })
  try {
    const result = runPublisher(state)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /missing the protocol directory payload: protocol\/host-capabilities-v1\.json/)
    assert.doesNotMatch(result.stdout, /wrangler.*object.*put/)
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

test('every versioned archive publishes an immutable sha256 sidecar', () => {
  const state = fixture()
  try {
    const result = runPublisher(state)
    assert.equal(result.status, 0, result.stderr)
    const keys = uploadKeys(result.stdout)
    for (const target of targets) {
      assert.equal(keys.includes(`beta/cli/unpeel-${workspaceVersion}-${target}.tar.gz.sha256`), true, target)
    }
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})
