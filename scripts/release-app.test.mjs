import assert from 'node:assert/strict'
import test from 'node:test'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { assertPublishableAppReleaseSource } from './release-source-state.mjs'

const commit = 'a'.repeat(40)
const cleanMain = {
  head: commit,
  branch: 'main',
  originMain: commit,
  remoteMain: commit,
  dirty: false,
  dirtyEntries: []
}
const dirtyMain = {
  ...cleanMain,
  dirty: true,
  dirtyEntries: [' M protocol/app-registry.json']
}

test('App publish refuses a dirty tree so the registry is never uploaded uncommitted', () => {
  assert.throws(
    () => assertPublishableAppReleaseSource(dirtyMain, { dryRun: false, allowDirty: false }),
    /clean worktree/
  )
  // Also enforces branch/origin alignment, exactly like the CLI gate.
  assert.throws(
    () => assertPublishableAppReleaseSource({ ...cleanMain, branch: 'feature' }, {}),
    /branch main/
  )
})

test('App publish passes on a clean, aligned main', () => {
  assert.doesNotThrow(() => assertPublishableAppReleaseSource(cleanMain, { dryRun: false, allowDirty: false }))
})

test('--dry-run is unaffected: a dirty tree still passes', () => {
  assert.doesNotThrow(() => assertPublishableAppReleaseSource(dirtyMain, { dryRun: true }))
})

test('--allow-dirty is the explicit escape hatch for a dirty tree', () => {
  assert.doesNotThrow(() => assertPublishableAppReleaseSource(dirtyMain, { allowDirty: true }))
})

test('batch publishing can defer the registry until every artifact is uploaded', () => {
  const stage = mkdtempSync(join(tmpdir(), 'unpeel-app-publish-'))
  try {
    const archive = join(stage, 'app.tar.gz')
    writeFileSync(archive, 'local dry-run artifact')
    const script = fileURLToPath(new URL('./release-app.mjs', import.meta.url))
    const version = JSON.parse(readFileSync(new URL('../protocol/app-registry.json', import.meta.url))).diffs.version
    const args = [script, '--app', 'diffs', '--version', version, '--channel', 'stable', '--dry-run', '--skip-build', '--linux-x86_64', archive, '--macos-universal', archive]
    for (const deferred of [false, true]) {
      const result = spawnSync(process.execPath, deferred ? [...args, '--skip-registry'] : args, { encoding: 'utf8' })
      assert.equal(result.status, 0, result.stderr)
      assert.match(result.stdout, /unpeel-diffs-latest-linux-x86_64.tar.gz/)
      assert.match(result.stdout, /unpeel-diffs-latest-macos-universal.tar.gz/)
      assert.equal(result.stdout.includes('stable/protocol/app-registry.json'), !deferred)
    }
  } finally {
    rmSync(stage, { recursive: true, force: true })
  }
})
