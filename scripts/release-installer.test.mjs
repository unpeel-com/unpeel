import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync
} from 'node:fs'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const repoRoot = resolve(import.meta.dirname, '..')
const installer = resolve(repoRoot, 'scripts/install.sh')

function fixture(binaries = ['unpeel', 'unpeel-host'], { withProtocol = false } = {}) {
  const root = mkdtempSync(resolve(tmpdir(), 'unpeel-installer-test-'))
  const payload = resolve(root, 'payload')
  const mockBin = resolve(root, 'bin')
  const installDir = resolve(root, 'install')
  mkdirSync(payload)
  mkdirSync(mockBin)
  for (const binary of binaries) {
    const path = resolve(payload, binary)
    writeFileSync(path, `#!/bin/sh\necho "${binary} 0.2.0"\n`)
    chmodSync(path, 0o755)
  }
  if (withProtocol) {
    mkdirSync(resolve(payload, 'protocol'))
    writeFileSync(resolve(payload, 'protocol', 'host-capabilities-v1.json'), '{}\n')
    mkdirSync(resolve(payload, 'generated'))
    writeFileSync(resolve(payload, 'generated', 'GeneratedRuntimeCatalog.swift'), '// fixture\n')
  }

  const archive = resolve(root, 'unpeel.tar.gz')
  const tar = spawnSync('tar', [
    '-czf', archive, '-C', payload, ...binaries, ...(withProtocol ? ['protocol', 'generated'] : [])
  ])
  assert.equal(tar.status, 0, tar.stderr?.toString())
  const digest = createHash('sha256').update(readFileSync(archive)).digest('hex')
  const sidecar = resolve(root, 'unpeel.tar.gz.sha256')
  writeFileSync(sidecar, `${digest}  unpeel-latest-test.tar.gz\n`)

  const curl = resolve(mockBin, 'curl')
  writeFileSync(curl, `#!/bin/sh
out=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
case "$url" in
  *.sha256) source_file="\${MOCK_SIDECAR:?}" ;;
  *) source_file="\${MOCK_ARCHIVE:?}" ;;
esac
[ -f "$source_file" ] || exit 22
cp "$source_file" "$out"
`)
  chmodSync(curl, 0o755)

  return { root, mockBin, installDir, archive, sidecar }
}

function runInstaller(state, sidecar = state.sidecar) {
  return spawnSync('sh', [installer], {
    encoding: 'utf8',
    env: {
      ...process.env,
      PATH: `${state.mockBin}:${process.env.PATH}`,
      HOME: state.root,
      UNPEEL_CHANNEL: 'beta',
      UNPEEL_INSTALL_BASE: 'https://release.invalid',
      UNPEEL_INSTALL_DIR: state.installDir,
      MOCK_ARCHIVE: state.archive,
      MOCK_SIDECAR: sidecar
    }
  })
}

test('installer requires a checksum sidecar before installing', () => {
  const state = fixture()
  try {
    const result = runInstaller(state, resolve(state.root, 'missing.sha256'))
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /checksum sidecar is unavailable/)
    assert.equal(existsSync(resolve(state.installDir, 'unpeel')), false)
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

test('installer rejects a malformed checksum sidecar', () => {
  const state = fixture()
  const malformed = resolve(state.root, 'malformed.sha256')
  writeFileSync(malformed, 'not-a-sha256  unpeel.tar.gz\n')
  try {
    const result = runInstaller(state, malformed)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /invalid checksum sidecar/)
    assert.equal(existsSync(resolve(state.installDir, 'unpeel')), false)
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

test('installer verifies a valid sidecar and installs both binaries', () => {
  const state = fixture()
  try {
    const result = runInstaller(state)
    assert.equal(result.status, 0, result.stderr)
    assert.equal(existsSync(resolve(state.installDir, 'unpeel')), true)
    assert.equal(existsSync(resolve(state.installDir, 'unpeel-host')), true)
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

// 0.4.5+ archives carry unpeel-attach; the installer installs it alongside.
// Older two-binary archives (the previous test) must keep installing.
test('installer installs unpeel-attach when the archive carries it', () => {
  const state = fixture(['unpeel', 'unpeel-host', 'unpeel-attach'])
  try {
    const result = runInstaller(state)
    assert.equal(result.status, 0, result.stderr)
    for (const binary of ['unpeel', 'unpeel-host', 'unpeel-attach']) {
      assert.equal(existsSync(resolve(state.installDir, binary)), true, binary)
    }
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})

test('installer ignores the protocol/ directory that 0.4.4+ archives carry', () => {
  const state = fixture(['unpeel', 'unpeel-host', 'unpeel-attach'], { withProtocol: true })
  try {
    const result = runInstaller(state)
    assert.equal(result.status, 0, result.stderr)
    for (const binary of ['unpeel', 'unpeel-host', 'unpeel-attach']) {
      assert.equal(existsSync(resolve(state.installDir, binary)), true, binary)
    }
    assert.equal(existsSync(resolve(state.installDir, 'protocol')), false, 'protocol/ is never installed')
    assert.equal(existsSync(resolve(state.installDir, 'generated')), false, 'generated/ is never installed')
  } finally {
    rmSync(state.root, { recursive: true, force: true })
  }
})
