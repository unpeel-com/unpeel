function requestOptions(method, timeoutMs) {
  return {
    method,
    cache: 'no-store',
    headers: { 'cache-control': 'no-cache' },
    ...(timeoutMs > 0 ? { signal: AbortSignal.timeout(timeoutMs) } : {})
  }
}

function root(baseUrl) {
  return baseUrl.replace(/\/$/, '')
}

const CLI_TARGETS = ['macos-universal', 'linux-x86_64', 'linux-aarch64']
// Every CLI archive ships these three binaries, in this order, plus the
// license payloads and BUILD_PROVENANCE.json, plus the `protocol/` directory
// (every `protocol/*` contract file: capability ledger, conformance fixtures,
// relay KAT vectors, app registry). The clients that pin a server release
// (the Apple repo's conformance tests, humans) read `protocol/` from the
// archive; `install.sh` ignores it.
export const CLI_BINARIES = Object.freeze(['unpeel', 'unpeel-host', 'unpeel-attach'])
export const CLI_ARCHIVE_PAYLOAD = Object.freeze([
  ...CLI_BINARIES,
  'LICENSE',
  'THIRD_PARTY_NOTICES.txt',
  'BUILD_PROVENANCE.json'
])
export const CLI_ARCHIVE_PROTOCOL_DIR = 'protocol'
// `generated/` carries the client-safe runtime catalog the Apple repo copies
// (GeneratedRuntimeCatalog.swift), so a pinned client needs no server
// checkout; install.sh ignores it like protocol/.
export const CLI_ARCHIVE_GENERATED_DIR = 'generated'
export const CLI_ARCHIVE_PROTOCOL_REQUIRED = Object.freeze([
  'protocol/host-capabilities-v1.json',
  'protocol/host-conformance-v1.json',
  'generated/GeneratedRuntimeCatalog.swift'
])

/**
 * Validate the normalized entry list of one CLI archive. Throws with the
 * first missing payload, so a publish never advertises an archive that a
 * pinned client cannot use.
 */
export function assertCliArchiveEntries(entries, target) {
  for (const required of CLI_ARCHIVE_PAYLOAD) {
    if (!entries.includes(required)) {
      throw new Error(`Tarball for ${target} is missing required release payload: ${required}`)
    }
  }
  for (const required of CLI_ARCHIVE_PROTOCOL_REQUIRED) {
    if (!entries.includes(required)) {
      throw new Error(
        `Tarball for ${target} is missing the ${required.split('/')[0]} directory payload: ${required} ` +
          '(every archive ships protocol/* and generated/* so pinned clients need no server checkout)'
      )
    }
  }
}
const ARTIFACT_REVISION_RE = /^[0-9a-f]{12}$/

function isObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}

export function isCompleteCliTargetSet(targets) {
  return CLI_TARGETS.every((target) => targets.includes(target))
}

export function validateCliArtifactRevision(value, sourceCommit) {
  if (value == null) return null
  const revision = String(value)
  if (!ARTIFACT_REVISION_RE.test(revision)) {
    throw new Error('--artifact-revision must be exactly 12 lowercase hexadecimal characters')
  }
  const expected = String(sourceCommit ?? '').slice(0, 12)
  if (revision !== expected) {
    throw new Error(
      `--artifact-revision ${revision} does not match current HEAD revision ${expected || '(unavailable)'}`
    )
  }
  return revision
}

export function assertCliArtifactRevisionPublish({
  artifactRevision,
  force,
  targets,
  version,
  publishedLatest,
  publishing
}) {
  if (artifactRevision == null) return
  if (force) {
    throw new Error('--artifact-revision cannot be combined with --force; revisioned keys are immutable')
  }
  if (!isCompleteCliTargetSet(targets)) {
    throw new Error('--artifact-revision requires all three target archives in one publish')
  }
  if (publishing && publishedLatest?.version !== version) {
    throw new Error(
      `--artifact-revision is only for recovering an already-published CLI ${version} manifest`
    )
  }
}

/**
 * Replacing latest.json with a new top-level version makes it authoritative,
 * so that first publish must be installable on every supported platform.
 * Partial uploads are safe only after a validated same-version manifest
 * exists and its other targets can be preserved.
 */
export function assertSafeCliTargetSet({
  version,
  targets,
  publishedLatest,
  artifactRevision = null
}) {
  if (
    publishedLatest?.version === version
    && publishedLatest.artifact_revision != null
    && artifactRevision == null
  ) {
    throw new Error(
      `CLI ${version} already uses revisioned immutable artifacts; ` +
        'publish a complete new --artifact-revision recovery or bump the semantic version'
    )
  }
  if (publishedLatest?.version === version || isCompleteCliTargetSet(targets)) return
  throw new Error(
    `Starting CLI ${version} requires all three target archives in one publish: ` +
      CLI_TARGETS.join(', ')
  )
}

export function cliLatestUrl(baseUrl, channel) {
  return `${root(baseUrl)}/releases/${channel}/cli/latest.json`
}

export function cliVersionedArtifactKey(channel, version, target, artifactRevision = null) {
  const revisionPart = artifactRevision == null ? '' : `-${artifactRevision}`
  return `${channel}/cli/unpeel-${version}${revisionPart}-${target}.tar.gz`
}

export function cliVersionedArtifactUrl(
  baseUrl,
  channel,
  version,
  target,
  artifactRevision = null
) {
  return `${root(baseUrl)}/releases/${cliVersionedArtifactKey(
    channel,
    version,
    target,
    artifactRevision
  )}`
}

function uncachedUrl(url) {
  const requestUrl = new URL(url)
  requestUrl.searchParams.set('release-preflight', String(Date.now()))
  return requestUrl.toString()
}

/**
 * Read the channel manifest used to preserve targets during a staged,
 * same-version publish. A missing manifest means this is the channel's first
 * CLI publish; every other response is unsafe to ignore because doing so could
 * erase target entries that are already live.
 */
export async function readPublishedCliLatest({
  fetchImpl = fetch,
  baseUrl,
  channel,
  timeoutMs = 10_000
}) {
  const url = cliLatestUrl(baseUrl, channel)
  const response = await fetchImpl(uncachedUrl(url), requestOptions('GET', timeoutMs))
  if (response.status === 404) return null
  if (!response.ok) {
    throw new Error(`could not read ${url}: HTTP ${response.status}`)
  }
  const latest = await response.json()
  if (!isObject(latest)) {
    throw new Error(`published CLI manifest at ${url} is not an object`)
  }
  if (typeof latest.version !== 'string' || latest.version.length === 0) {
    throw new Error(`published CLI manifest at ${url} has no version`)
  }
  if (latest.channel != null && latest.channel !== channel) {
    throw new Error(
      `published CLI manifest at ${url} says channel ${JSON.stringify(latest.channel)}, expected ${channel}`
    )
  }
  if (!isObject(latest.targets)) {
    throw new Error(`published CLI manifest at ${url} has no valid targets object`)
  }
  const artifactRevision = latest.artifact_revision ?? null
  if (
    artifactRevision != null
    && (typeof artifactRevision !== 'string' || !ARTIFACT_REVISION_RE.test(artifactRevision))
  ) {
    throw new Error(`published CLI manifest at ${url} has an invalid artifact_revision`)
  }
  if (artifactRevision != null && !isCompleteCliTargetSet(Object.keys(latest.targets))) {
    throw new Error(`published CLI manifest at ${url} has an incomplete revisioned target set`)
  }
  for (const [target, entry] of Object.entries(latest.targets)) {
    if (!CLI_TARGETS.includes(target)) {
      throw new Error(`published CLI manifest at ${url} has unsupported target ${target}`)
    }
    const expectedKey = cliVersionedArtifactKey(
      channel,
      latest.version,
      target,
      artifactRevision
    )
    const expectedLatestKey = `${channel}/cli/unpeel-latest-${target}.tar.gz`
    const expectedSidecarKey = `${expectedKey}.sha256`
    if (
      !isObject(entry)
      || entry.key !== expectedKey
      || entry.latest_key !== expectedLatestKey
      || !Number.isSafeInteger(entry.bytes)
      || entry.bytes <= 0
      || typeof entry.sha256 !== 'string'
      || !/^[0-9a-f]{64}$/.test(entry.sha256)
    ) {
      throw new Error(`published CLI manifest at ${url} has invalid metadata for ${target}`)
    }
    if (
      artifactRevision != null
      && (
        entry.sidecar_key !== expectedSidecarKey
        || entry.sidecar_path !== `/releases/${expectedSidecarKey}`
        || entry.sidecar_url !== `${root(baseUrl)}/releases/${expectedSidecarKey}`
      )
    ) {
      throw new Error(`published CLI manifest at ${url} has invalid revision sidecar metadata for ${target}`)
    }
  }
  return latest
}

/**
 * Check immutable object URLs directly. latest.json may be incomplete after an
 * older partial publish, so it is not a safe source of truth for overwrite
 * protection.
 */
export async function findPublishedCliArtifacts({
  fetchImpl = fetch,
  baseUrl,
  channel,
  version,
  targets,
  artifactRevision = null,
  timeoutMs = 10_000
}) {
  const checks = await Promise.all(targets.flatMap((target) => {
    const artifactUrl = cliVersionedArtifactUrl(
      baseUrl,
      channel,
      version,
      target,
      artifactRevision
    )
    const candidates = artifactRevision == null
      ? [{ target, url: artifactUrl }]
      : [
          { target, kind: 'archive', url: artifactUrl },
          { target, kind: 'sidecar', url: `${artifactUrl}.sha256` }
        ]
    return candidates.map(async (candidate) => {
      const response = await fetchImpl(
        uncachedUrl(candidate.url),
        requestOptions('HEAD', timeoutMs)
      )
      if (response.status === 404) return null
      if (!response.ok) {
        throw new Error(`could not verify ${candidate.url}: HTTP ${response.status}`)
      }
      return candidate
    })
  }))
  return checks.filter(Boolean)
}

/**
 * Preserve entries from earlier platform uploads of the same release. A new
 * version intentionally starts a new target set: carrying an older version's
 * artifacts under a newer top-level version would make update metadata lie.
 */
export function mergeCliLatest({
  channel,
  version,
  artifactRevision = null,
  publishedAt,
  publishedLatest,
  newTargets
}) {
  const previousTargets =
    artifactRevision == null
      && publishedLatest
      && publishedLatest.version === version
      && publishedLatest.artifact_revision == null
      && (publishedLatest.channel == null || publishedLatest.channel === channel)
      && publishedLatest.targets
      && typeof publishedLatest.targets === 'object'
      && !Array.isArray(publishedLatest.targets)
      ? publishedLatest.targets
      : {}

  return {
    channel,
    version,
    ...(artifactRevision == null ? {} : { artifact_revision: artifactRevision }),
    published_at: publishedAt,
    targets: { ...previousTargets, ...newTargets }
  }
}
