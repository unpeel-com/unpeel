import assert from 'node:assert/strict'
import test from 'node:test'

import {
  assertCliArchiveEntries,
  assertCliArtifactRevisionPublish,
  assertSafeCliTargetSet,
  cliVersionedArtifactKey,
  cliVersionedArtifactUrl,
  findPublishedCliArtifacts,
  isCompleteCliTargetSet,
  mergeCliLatest,
  readPublishedCliLatest,
  validateCliArtifactRevision
} from './release-cli-state.mjs'

function response(status, body) {
  return {
    status,
    ok: status >= 200 && status < 300,
    json: async () => body
  }
}

test('same-version platform uploads preserve existing manifest targets', () => {
  const latest = mergeCliLatest({
    channel: 'beta',
    version: '0.2.0',
    publishedAt: '2026-08-14T10:00:00.000Z',
    publishedLatest: {
      channel: 'beta',
      version: '0.2.0',
      targets: { 'macos-universal': { sha256: 'mac' } }
    },
    newTargets: {
      'linux-x86_64': { sha256: 'linux' }
    }
  })

  assert.deepEqual(latest.targets, {
    'macos-universal': { sha256: 'mac' },
    'linux-x86_64': { sha256: 'linux' }
  })
})

test('a version bump never advertises targets from the older version', () => {
  const latest = mergeCliLatest({
    channel: 'beta',
    version: '0.3.0',
    publishedAt: '2026-08-14T10:00:00.000Z',
    publishedLatest: {
      channel: 'beta',
      version: '0.2.0',
      targets: { 'macos-universal': { sha256: 'old' } }
    },
    newTargets: {
      'linux-aarch64': { sha256: 'new' }
    }
  })

  assert.deepEqual(latest.targets, {
    'linux-aarch64': { sha256: 'new' }
  })
})

test('revision recovery replaces the complete target set and records its source revision', () => {
  const revision = 'abcdef012345'
  const latest = mergeCliLatest({
    channel: 'beta',
    version: '0.2.0',
    artifactRevision: revision,
    publishedAt: '2026-08-14T10:00:00.000Z',
    publishedLatest: {
      channel: 'beta',
      version: '0.2.0',
      targets: { 'macos-universal': { sha256: 'old' } }
    },
    newTargets: {
      'macos-universal': { sha256: 'mac' },
      'linux-x86_64': { sha256: 'x86' },
      'linux-aarch64': { sha256: 'arm' }
    }
  })

  assert.equal(latest.artifact_revision, revision)
  assert.deepEqual(latest.targets, {
    'macos-universal': { sha256: 'mac' },
    'linux-x86_64': { sha256: 'x86' },
    'linux-aarch64': { sha256: 'arm' }
  })
})

test('artifact revisions are exact lowercase prefixes of current HEAD', () => {
  const head = `${'abcdef012345'}${'6'.repeat(28)}`
  assert.equal(validateCliArtifactRevision(null, head), null)
  assert.equal(validateCliArtifactRevision('abcdef012345', head), 'abcdef012345')
  for (const invalid of ['ABCDEF012345', 'abcdef01234', 'abcdef0123456', 'release-1']) {
    assert.throws(() => validateCliArtifactRevision(invalid, head), /12 lowercase hexadecimal/)
  }
  assert.throws(
    () => validateCliArtifactRevision('012345abcdef', head),
    /does not match current HEAD revision/
  )
})

test('revision recovery requires a complete same-version replacement without force', () => {
  const base = {
    artifactRevision: 'abcdef012345',
    force: false,
    targets: ['macos-universal', 'linux-x86_64', 'linux-aarch64'],
    version: '0.2.0',
    publishedLatest: { channel: 'beta', version: '0.2.0', targets: {} },
    publishing: true
  }
  assert.doesNotThrow(() => assertCliArtifactRevisionPublish(base))
  assert.throws(
    () => assertCliArtifactRevisionPublish({ ...base, force: true }),
    /cannot be combined with --force/
  )
  assert.throws(
    () => assertCliArtifactRevisionPublish({ ...base, targets: ['macos-universal'] }),
    /requires all three target archives/
  )
  assert.throws(
    () => assertCliArtifactRevisionPublish({ ...base, publishedLatest: null }),
    /only for recovering an already-published CLI 0\.2\.0 manifest/
  )
  assert.throws(
    () => assertCliArtifactRevisionPublish({
      ...base,
      publishedLatest: { channel: 'beta', version: '0.1.0', targets: {} }
    }),
    /only for recovering an already-published CLI 0\.2\.0 manifest/
  )
})

test('revisioned artifact URLs are unique while normal keys stay unchanged', () => {
  assert.equal(
    cliVersionedArtifactKey('beta', '0.2.0', 'linux-x86_64'),
    'beta/cli/unpeel-0.2.0-linux-x86_64.tar.gz'
  )
  assert.equal(
    cliVersionedArtifactUrl(
      'https://unpeel.com/',
      'beta',
      '0.2.0',
      'linux-x86_64',
      'abcdef012345'
    ),
    'https://unpeel.com/releases/beta/cli/unpeel-0.2.0-abcdef012345-linux-x86_64.tar.gz'
  )
})

test('immutable object checks catch an artifact omitted from latest.json', async () => {
  const fetchImpl = async (url, options) => {
    assert.equal(options.method, 'HEAD')
    return response(url.includes('linux-x86_64') ? 200 : 404)
  }

  const found = await findPublishedCliArtifacts({
    fetchImpl,
    baseUrl: 'https://unpeel.com',
    channel: 'alpha',
    version: '0.2.0',
    targets: ['linux-x86_64', 'linux-aarch64'],
    timeoutMs: 0
  })

  assert.deepEqual(found, [{
    target: 'linux-x86_64',
    url: 'https://unpeel.com/releases/alpha/cli/unpeel-0.2.0-linux-x86_64.tar.gz'
  }])
})

test('unexpected object-check responses fail closed', async () => {
  await assert.rejects(
    findPublishedCliArtifacts({
      fetchImpl: async () => response(503),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      version: '0.2.0',
      targets: ['macos-universal'],
      timeoutMs: 0
    }),
    /HTTP 503/
  )
})

test('revision preflight checks both immutable archive and sidecar', async () => {
  const requested = []
  const found = await findPublishedCliArtifacts({
    fetchImpl: async (url, options) => {
      assert.equal(options.method, 'HEAD')
      const cleanUrl = new URL(url)
      cleanUrl.search = ''
      requested.push(cleanUrl.toString())
      return response(new URL(url).pathname.endsWith('.sha256') ? 200 : 404)
    },
    baseUrl: 'https://unpeel.com',
    channel: 'beta',
    version: '0.2.0',
    artifactRevision: 'abcdef012345',
    targets: ['linux-aarch64'],
    timeoutMs: 0
  })

  assert.deepEqual(requested, [
    'https://unpeel.com/releases/beta/cli/unpeel-0.2.0-abcdef012345-linux-aarch64.tar.gz',
    'https://unpeel.com/releases/beta/cli/unpeel-0.2.0-abcdef012345-linux-aarch64.tar.gz.sha256'
  ])
  assert.deepEqual(found, [{
    target: 'linux-aarch64',
    kind: 'sidecar',
    url: 'https://unpeel.com/releases/beta/cli/unpeel-0.2.0-abcdef012345-linux-aarch64.tar.gz.sha256'
  }])
})

test('a missing channel manifest is a clean first publish', async () => {
  const latest = await readPublishedCliLatest({
    fetchImpl: async (_url, options) => {
      assert.equal(options.method, 'GET')
      return response(404)
    },
    baseUrl: 'https://unpeel.com/',
    channel: 'beta',
    timeoutMs: 0
  })

  assert.equal(latest, null)
})

test('a malformed channel manifest fails closed instead of erasing targets', async () => {
  await assert.rejects(
    readPublishedCliLatest({
      fetchImpl: async () => response(200, { channel: 'beta', version: '0.2.0' }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /no valid targets object/
  )
})

test('published target metadata is validated before it can be preserved', async () => {
  const validTarget = {
    key: 'beta/cli/unpeel-0.2.0-macos-universal.tar.gz',
    latest_key: 'beta/cli/unpeel-latest-macos-universal.tar.gz',
    bytes: 123,
    sha256: 'a'.repeat(64)
  }
  const latest = await readPublishedCliLatest({
    fetchImpl: async () => response(200, {
      channel: 'beta',
      version: '0.2.0',
      targets: { 'macos-universal': validTarget }
    }),
    baseUrl: 'https://unpeel.com',
    channel: 'beta',
    timeoutMs: 0
  })
  assert.equal(latest.targets['macos-universal'].bytes, 123)

  await assert.rejects(
    readPublishedCliLatest({
      fetchImpl: async () => response(200, {
        channel: 'beta',
        version: '0.2.0',
        targets: { 'linux-x86_64': { ...validTarget, sha256: 'not-a-digest' } }
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /invalid metadata for linux-x86_64/
  )
})

test('revisioned manifests bind every target and sidecar to the top-level revision', async () => {
  const revision = 'abcdef012345'
  const target = 'linux-x86_64'
  const validTargets = {}
  for (const currentTarget of ['macos-universal', 'linux-x86_64', 'linux-aarch64']) {
    const key = `beta/cli/unpeel-0.2.0-${revision}-${currentTarget}.tar.gz`
    validTargets[currentTarget] = {
      key,
      latest_key: `beta/cli/unpeel-latest-${currentTarget}.tar.gz`,
      sidecar_key: `${key}.sha256`,
      sidecar_path: `/releases/${key}.sha256`,
      sidecar_url: `https://unpeel.com/releases/${key}.sha256`,
      bytes: 123,
      sha256: 'a'.repeat(64)
    }
  }
  const body = {
    channel: 'beta',
    version: '0.2.0',
    artifact_revision: revision,
    targets: validTargets
  }
  const latest = await readPublishedCliLatest({
    fetchImpl: async () => response(200, body),
    baseUrl: 'https://unpeel.com',
    channel: 'beta',
    timeoutMs: 0
  })
  assert.equal(latest.artifact_revision, revision)

  await assert.rejects(
    readPublishedCliLatest({
      fetchImpl: async () => response(200, {
        ...body,
        targets: {
          ...validTargets,
          [target]: {
            ...validTargets[target],
            key: `beta/cli/unpeel-0.2.0-${target}.tar.gz`
          }
        }
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /invalid metadata for linux-x86_64/
  )
  await assert.rejects(
    readPublishedCliLatest({
      fetchImpl: async () => response(200, {
        ...body,
        targets: {
          ...validTargets,
          [target]: { ...validTargets[target], sidecar_key: 'wrong' }
        }
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /invalid revision sidecar metadata for linux-x86_64/
  )
  await assert.rejects(
    readPublishedCliLatest({
      fetchImpl: async () => response(200, {
        ...body,
        artifact_revision: 'ABCDEF012345'
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /invalid artifact_revision/
  )
  await assert.rejects(
    readPublishedCliLatest({
      fetchImpl: async () => response(200, {
        ...body,
        targets: { [target]: validTargets[target] }
      }),
      baseUrl: 'https://unpeel.com',
      channel: 'beta',
      timeoutMs: 0
    }),
    /incomplete revisioned target set/
  )
})

test('unread manifest recovery requires a complete three-target replacement', () => {
  assert.equal(isCompleteCliTargetSet(['macos-universal', 'linux-x86_64']), false)
  assert.equal(isCompleteCliTargetSet([
    'linux-aarch64',
    'macos-universal',
    'linux-x86_64'
  ]), true)
})

test('the first publish of a CLI version requires all three targets', () => {
  assert.throws(
    () => assertSafeCliTargetSet({
      version: '0.2.0',
      targets: ['macos-universal'],
      publishedLatest: null
    }),
    /requires all three target archives/
  )
  assert.throws(
    () => assertSafeCliTargetSet({
      version: '0.2.0',
      targets: ['linux-x86_64', 'linux-aarch64'],
      publishedLatest: { version: '0.1.0', targets: {} }
    }),
    /requires all three target archives/
  )
  assert.doesNotThrow(() => assertSafeCliTargetSet({
    version: '0.2.0',
    targets: ['macos-universal', 'linux-x86_64', 'linux-aarch64'],
    publishedLatest: null
  }))
})

test('a validated same-version manifest permits a partial follow-up', () => {
  assert.doesNotThrow(() => assertSafeCliTargetSet({
    version: '0.2.0',
    targets: ['linux-aarch64'],
    publishedLatest: {
      channel: 'beta',
      version: '0.2.0',
      targets: { 'macos-universal': { sha256: 'mac' } }
    }
  }))
})

test('a revisioned same-version manifest rejects every later normal publish', () => {
  for (const targets of [
    ['linux-aarch64'],
    ['macos-universal', 'linux-x86_64', 'linux-aarch64']
  ]) {
    assert.throws(
      () => assertSafeCliTargetSet({
        version: '0.2.0',
        targets,
        artifactRevision: null,
        publishedLatest: {
          channel: 'beta',
          version: '0.2.0',
          artifact_revision: 'abcdef012345',
          targets: {}
        }
      }),
      /already uses revisioned immutable artifacts/
    )
  }
  assert.doesNotThrow(() => assertSafeCliTargetSet({
    version: '0.3.0',
    targets: ['macos-universal', 'linux-x86_64', 'linux-aarch64'],
    artifactRevision: null,
    publishedLatest: {
      channel: 'beta',
      version: '0.2.0',
      artifact_revision: 'abcdef012345',
      targets: {}
    }
  }))
})

test('archive entry check requires the three binaries, the payloads, and protocol/', () => {
  const complete = [
    'unpeel', 'unpeel-host', 'unpeel-attach', 'LICENSE', 'THIRD_PARTY_NOTICES.txt',
    'BUILD_PROVENANCE.json', 'protocol', 'protocol/host-capabilities-v1.json',
    'protocol/host-conformance-v1.json', 'protocol/relay-kat-vectors-v1.json',
    'generated', 'generated/GeneratedRuntimeCatalog.swift'
  ]
  assert.doesNotThrow(() => assertCliArchiveEntries(complete, 'macos-universal'))
  assert.throws(
    () => assertCliArchiveEntries(complete.filter((entry) => entry !== 'unpeel-attach'), 'linux-x86_64'),
    /missing required release payload: unpeel-attach/
  )
  assert.throws(
    () => assertCliArchiveEntries(complete.filter((entry) => !entry.startsWith('protocol')), 'linux-aarch64'),
    /missing the protocol directory payload: protocol\/host-capabilities-v1\.json/
  )
  assert.throws(
    () => assertCliArchiveEntries(complete.filter((entry) => !entry.startsWith('generated')), 'linux-aarch64'),
    /missing the generated directory payload: generated\/GeneratedRuntimeCatalog\.swift/
  )
})
