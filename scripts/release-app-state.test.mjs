import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import test from 'node:test'
import { resolveChangelogPath } from './release-changelog.mjs'

import {
  findPublishedAppArtifacts,
  mergeAppLatest,
  planAppRelease,
  readPublishedAppLatest
} from './release-app-state.mjs'

const repoRoot = resolve(import.meta.dirname, '..')

function artifact(channel, key, overrides = {}) {
  return {
    key,
    path: `/releases/${key}`,
    url: `https://unpeel.com/releases/${key}`,
    filename: key.split('/').at(-1),
    bytes: 123,
    sha256: 'a'.repeat(64),
    ...overrides
  }
}

function response(status, body) {
  return {
    status,
    ok: status >= 200 && status < 300,
    json: async () => body
  }
}

test('direct checks catch immutable artifacts omitted from latest.json', async () => {
  const found = await findPublishedAppArtifacts({
    fetchImpl: async (url, options) => {
      assert.equal(options.method, 'HEAD')
      return response(url.includes('.zip') ? 200 : 404)
    },
    baseUrl: 'https://unpeel.com',
    channel: 'beta',
    version: '0.2.0',
    artifactKinds: ['dmg', 'zip'],
    timeoutMs: 0
  })

  assert.deepEqual(found, [{
    kind: 'zip',
    url: 'https://unpeel.com/releases/beta/Unpeel-0.2.0.zip'
  }])
})

test('manifest HTTP and shape errors fail closed', async () => {
  await assert.rejects(
    readPublishedAppLatest({
      fetchImpl: async () => response(503),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /HTTP 503/
  )
  await assert.rejects(
    readPublishedAppLatest({
      fetchImpl: async () => response(200, { channel: 'beta', version: '0.1.0' }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /valid positive build/
  )
})

test('published artifact metadata is fully validated before preservation', async () => {
  const dmg = artifact('beta', 'beta/Unpeel-0.2.0.dmg')
  const latestDmg = artifact('beta', 'beta/Unpeel-latest.dmg')
  const zip = artifact('beta', 'beta/Unpeel-0.2.0.zip', { sha256: 'b'.repeat(64) })
  const latestZip = artifact('beta', 'beta/Unpeel-latest.zip', { sha256: 'b'.repeat(64) })
  const manifest = {
    channel: 'beta',
    version: '0.2.0',
    build: '34',
    dmg,
    latest_dmg: latestDmg,
    zip,
    latest_zip: latestZip,
    appcast: artifact('beta', 'beta/appcast.xml', { sha256: 'c'.repeat(64) })
  }

  await assert.doesNotReject(readPublishedAppLatest({
    fetchImpl: async () => response(200, manifest),
    baseUrl: 'https://unpeel.com',
    channel: 'beta',
    timeoutMs: 0
  }))

  await assert.rejects(
    readPublishedAppLatest({
      fetchImpl: async () => response(200, {
        ...manifest,
        dmg: { ...dmg, sha256: 'not-a-digest' }
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /invalid metadata for dmg/
  )
  await assert.rejects(
    readPublishedAppLatest({
      fetchImpl: async () => response(200, {
        ...manifest,
        latest_dmg: { ...latestDmg, bytes: latestDmg.bytes + 1 }
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /inconsistent dmg\/latest_dmg/
  )
  const { latest_dmg: _omitted, ...missingMutableAlias } = manifest
  await assert.rejects(
    readPublishedAppLatest({
      fetchImpl: async () => response(200, missingMutableAlias),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /must contain dmg and latest_dmg together/
  )
})

test('new-version partial publishes are rejected before latest.json can be clobbered', () => {
  for (const force of [false, true]) {
    assert.throws(
      () => planAppRelease({
        channel: 'beta',
        version: '0.2.0',
        build: '34',
        artifactKinds: [],
        publishedByChannel: {
          alpha: null,
          beta: { channel: 'beta', version: '0.1.0-beta.33', build: '33' },
          stable: null
        },
        force
      }),
      /requires both --dmg and --zip/
    )
  }
})

test('same-version appcast-only publish preserves download metadata', () => {
  const publishedLatest = {
    channel: 'beta',
    version: '0.2.0',
    build: '34',
    dmg: { key: 'beta/Unpeel-0.2.0.dmg' },
    latest_dmg: { key: 'beta/Unpeel-latest.dmg' },
    zip: { key: 'beta/Unpeel-0.2.0.zip' },
    latest_zip: { key: 'beta/Unpeel-latest.zip' },
    appcast: { key: 'beta/appcast.xml', sha256: 'old' }
  }
  const plan = planAppRelease({
    channel: 'beta',
    version: '0.2.0',
    artifactKinds: [],
    publishedByChannel: { alpha: null, beta: publishedLatest, stable: null }
  })
  const merged = mergeAppLatest({
    channel: 'beta',
    version: '0.2.0',
    build: plan.build,
    publishedAt: '2026-08-14T12:00:00.000Z',
    publishedLatest: plan.publishedLatest,
    newFields: { appcast: { key: 'beta/appcast.xml', sha256: 'new' } }
  })

  assert.equal(merged.build, '34')
  assert.equal(merged.dmg.key, 'beta/Unpeel-0.2.0.dmg')
  assert.equal(merged.zip.key, 'beta/Unpeel-0.2.0.zip')
  assert.equal(merged.appcast.sha256, 'new')
})

test('force cannot change a same-version build while preserving old downloads', () => {
  const publishedLatest = {
    channel: 'beta',
    version: '0.2.0',
    build: '34',
    dmg: { key: 'beta/Unpeel-0.2.0.dmg' },
    latest_dmg: { key: 'beta/Unpeel-latest.dmg' },
    zip: { key: 'beta/Unpeel-0.2.0.zip' },
    latest_zip: { key: 'beta/Unpeel-latest.zip' }
  }
  assert.throws(
    () => planAppRelease({
      channel: 'beta',
      version: '0.2.0',
      build: '35',
      artifactKinds: [],
      publishedByChannel: { alpha: null, beta: publishedLatest, stable: null },
      force: true
    }),
    /requires --force with both --dmg and --zip/
  )
  assert.doesNotThrow(() => planAppRelease({
    channel: 'beta',
    version: '0.2.0',
    build: '35',
    artifactKinds: ['dmg', 'zip'],
    publishedByChannel: { alpha: null, beta: publishedLatest, stable: null },
    force: true
  }))
})

test('new builds are monotonic across every channel', () => {
  assert.throws(
    () => planAppRelease({
      channel: 'stable',
      version: '0.2.0',
      build: '34',
      artifactKinds: ['dmg', 'zip'],
      publishedByChannel: {
        alpha: { channel: 'alpha', version: '0.3.0-alpha.1', build: '35' },
        beta: { channel: 'beta', version: '0.1.0-beta.33', build: '33' },
        stable: null
      }
    }),
    /not greater than build 35.*alpha/
  )
})

test('native release entrypoint rejects build zero before doing release work', () => {
  const result = spawnSync('bash', [
    resolve(repoRoot, 'apps/native/release.sh'),
    '--channel', 'beta',
    '--build', '0',
    '--dry-run'
  ], { encoding: 'utf8' })

  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /--build must be a positive integer/)
  assert.doesNotMatch(result.stdout, /building \+ signing Unpeel\.app/)
})

test('app publisher rejects misspelled and equals-form dry-run flags', () => {
  for (const flag of ['--dryrun', '--dry-run=true']) {
    const result = spawnSync('node', [
      resolve(repoRoot, 'scripts/publish-cloudflare-release.mjs'),
      flag
    ], { encoding: 'utf8' })

    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /Unknown option|Unexpected argument form/)
    assert.doesNotMatch(result.stdout, /wrangler.*object.*put/)
  }
})

test('CLI publisher rejects misspelled and equals-form dry-run flags', () => {
  for (const flag of ['--dryrun', '--dry-run=true']) {
    const result = spawnSync('node', [
      resolve(repoRoot, 'scripts/release-cli.mjs'),
      flag
    ], { encoding: 'utf8' })

    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /Unknown option|Unexpected argument form/)
    assert.doesNotMatch(result.stdout, /wrangler.*object.*put/)
  }
})

test('the app release reads the changelog from the website sibling, then the monorepo, then fails', () => {
  const root = '/work/unpeel'
  const sibling = resolve(root, '..', 'unpeel-website', 'app', 'changelog.md')
  const monorepo = resolve(root, 'apps', 'website', 'app', 'changelog.md')
  const present = (paths) => (path) => paths.includes(path)

  assert.deepEqual(
    resolveChangelogPath({ repoRoot: root, env: {}, exists: present([sibling, monorepo]) }),
    { path: sibling, source: 'website sibling' }
  )
  assert.deepEqual(
    resolveChangelogPath({ repoRoot: root, env: {}, exists: present([monorepo]) }),
    { path: monorepo, source: 'monorepo' }
  )
  assert.throws(
    () => resolveChangelogPath({ repoRoot: root, env: {}, exists: present([]) }),
    /Clone unpeel-website next to this repo, or set UNPEEL_CHANGELOG/
  )
  assert.deepEqual(
    resolveChangelogPath({
      repoRoot: root,
      env: { UNPEEL_CHANGELOG: '/elsewhere/changelog.md' },
      exists: present(['/elsewhere/changelog.md'])
    }),
    { path: '/elsewhere/changelog.md', source: 'override' }
  )
  assert.throws(
    () => resolveChangelogPath({ repoRoot: root, env: { UNPEEL_CHANGELOG: '/gone.md' }, exists: present([]) }),
    /UNPEEL_CHANGELOG points at a missing file/
  )
})

test('the changelog resolver CLI prints the resolved path and fails without a changelog', () => {
  // The website is a separate repository, so a checkout of this one may or
  // may not have a `../unpeel-website` sibling: drive the CLI through the
  // UNPEEL_CHANGELOG override so the test is independent of the machine.
  const dir = mkdtempSync(resolve(tmpdir(), 'unpeel-changelog-'))
  const changelog = resolve(dir, 'changelog.md')
  writeFileSync(changelog, '## 0.0.0 — test\n')
  const ok = spawnSync('node', [
    resolve(repoRoot, 'scripts/release-changelog.mjs'),
    '--repo-root', repoRoot
  ], { encoding: 'utf8', env: { ...process.env, UNPEEL_CHANGELOG: changelog } })
  assert.equal(ok.status, 0, ok.stderr)
  assert.equal(ok.stdout.trim(), changelog)

  const missing = spawnSync('node', [
    resolve(repoRoot, 'scripts/release-changelog.mjs'),
    '--repo-root', repoRoot
  ], { encoding: 'utf8', env: { ...process.env, UNPEEL_CHANGELOG: resolve(dir, 'gone.md') } })
  assert.equal(missing.status, 1)
  assert.match(missing.stderr, /UNPEEL_CHANGELOG points at a missing file/)
  rmSync(dir, { recursive: true, force: true })
})
