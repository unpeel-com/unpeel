import assert from 'node:assert/strict'
import test from 'node:test'

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
