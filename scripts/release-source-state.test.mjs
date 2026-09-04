import assert from 'node:assert/strict'
import test from 'node:test'

import {
  assertPublishableReleaseSource,
  cliBuildProvenance,
  validateCliBinaryTarget,
  validateCliBuildProvenance
} from './release-source-state.mjs'

const commit = 'a'.repeat(40)
const cleanMain = {
  head: commit,
  branch: 'main',
  originMain: commit,
  remoteMain: commit,
  dirty: false,
  dirtyEntries: []
}

test('real releases require clean main aligned with origin/main', () => {
  assert.doesNotThrow(() => assertPublishableReleaseSource(cleanMain))
  assert.throws(
    () => assertPublishableReleaseSource({ ...cleanMain, branch: 'feature' }),
    /branch main/
  )
  assert.throws(
    () => assertPublishableReleaseSource({ ...cleanMain, dirty: true, dirtyEntries: [' M file'] }),
    /clean worktree/
  )
  assert.throws(
    () => assertPublishableReleaseSource({ ...cleanMain, originMain: 'b'.repeat(40) }),
    /does not match/
  )
  assert.throws(
    () => assertPublishableReleaseSource({ ...cleanMain, remoteMain: 'b'.repeat(40) }),
    /remote origin\/main/
  )
})

test('CLI provenance binds every archive to version, target, and current source', () => {
  const provenance = cliBuildProvenance({
    state: cleanMain,
    version: '0.2.0',
    target: 'linux-aarch64'
  })
  assert.doesNotThrow(() => validateCliBuildProvenance({
    provenance,
    version: '0.2.0',
    target: 'linux-aarch64',
    sourceState: cleanMain,
    publishing: true
  }))
  assert.throws(
    () => validateCliBuildProvenance({
      provenance,
      version: '0.2.0',
      target: 'linux-x86_64',
      sourceState: cleanMain,
      publishing: true
    }),
    /invalid BUILD_PROVENANCE/
  )
  assert.throws(
    () => validateCliBuildProvenance({
      provenance: { ...provenance, source_commit: 'b'.repeat(40) },
      version: '0.2.0',
      target: 'linux-aarch64',
      sourceState: cleanMain,
      publishing: true
    }),
    /not current HEAD/
  )
  assert.throws(
    () => validateCliBuildProvenance({
      provenance: { ...provenance, source_dirty: true },
      version: '0.2.0',
      target: 'linux-aarch64',
      sourceState: cleanMain,
      publishing: true
    }),
    /dirty worktree/
  )
})

test('CLI binary headers must match the archive target', () => {
  const x86Elf = Buffer.alloc(64)
  x86Elf.set([0x7f, 0x45, 0x4c, 0x46, 2, 1])
  x86Elf.writeUInt16LE(62, 18)
  assert.doesNotThrow(() => validateCliBinaryTarget({
    header: x86Elf,
    target: 'linux-x86_64',
    binary: 'unpeel'
  }))
  assert.throws(
    () => validateCliBinaryTarget({
      header: x86Elf,
      target: 'linux-aarch64',
      binary: 'unpeel'
    }),
    /ELF machine 62, expected 183/
  )

  const universal = Buffer.alloc(48)
  universal.writeUInt32BE(0xcafebabe, 0)
  universal.writeUInt32BE(2, 4)
  universal.writeUInt32BE(0x01000007, 8)
  universal.writeUInt32BE(0x0100000c, 28)
  assert.doesNotThrow(() => validateCliBinaryTarget({
    header: universal,
    target: 'macos-universal',
    binary: 'unpeel-host'
  }))
  universal.writeUInt32BE(0x01000007, 28)
  assert.throws(
    () => validateCliBinaryTarget({
      header: universal,
      target: 'macos-universal',
      binary: 'unpeel-host'
    }),
    /both arm64 and x86_64 slices/
  )
})
