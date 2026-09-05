const RELEASE_CHANNELS = ['alpha', 'beta', 'stable']
const APP_ARTIFACT_FIELDS = ['dmg', 'latest_dmg', 'zip', 'latest_zip', 'appcast']

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

function uncachedUrl(url) {
  const requestUrl = new URL(url)
  requestUrl.searchParams.set('release-preflight', String(Date.now()))
  return requestUrl.toString()
}

function isObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}

function positiveBuild(value) {
  const text = String(value ?? '')
  return /^[1-9][0-9]*$/.test(text) ? text : null
}

export function appLatestUrl(baseUrl, channel) {
  return `${root(baseUrl)}/releases/${channel}/latest.json`
}

export function appVersionedArtifactUrl(baseUrl, channel, version, kind) {
  if (kind !== 'dmg' && kind !== 'zip') {
    throw new Error(`unsupported immutable app artifact kind: ${kind}`)
  }
  return `${root(baseUrl)}/releases/${channel}/Unpeel-${version}.${kind}`
}

export async function readPublishedAppLatest({
  fetchImpl = fetch,
  baseUrl,
  channel,
  timeoutMs = 10_000
}) {
  const url = appLatestUrl(baseUrl, channel)
  const response = await fetchImpl(uncachedUrl(url), requestOptions('GET', timeoutMs))
  if (response.status === 404) return null
  if (!response.ok) throw new Error(`could not read ${url}: HTTP ${response.status}`)

  const latest = await response.json()
  if (!isObject(latest)) {
    throw new Error(`published app manifest at ${url} is not an object`)
  }
  if (latest.channel != null && latest.channel !== channel) {
    throw new Error(
      `published app manifest at ${url} says channel ${JSON.stringify(latest.channel)}, expected ${channel}`
    )
  }
  if (typeof latest.version !== 'string' || latest.version.length === 0) {
    throw new Error(`published app manifest at ${url} has no version`)
  }
  if (!positiveBuild(latest.build)) {
    throw new Error(`published app manifest at ${url} has no valid positive build`)
  }
  const expectedKeys = {
    dmg: `${channel}/Unpeel-${latest.version}.dmg`,
    latest_dmg: `${channel}/Unpeel-latest.dmg`,
    zip: `${channel}/Unpeel-${latest.version}.zip`,
    latest_zip: `${channel}/Unpeel-latest.zip`,
    appcast: `${channel}/appcast.xml`
  }
  for (const field of APP_ARTIFACT_FIELDS) {
    const entry = latest[field]
    if (entry == null) continue
    const expectedKey = expectedKeys[field]
    if (
      !isObject(entry)
      || entry.key !== expectedKey
      || entry.path !== `/releases/${expectedKey}`
      || entry.url !== `${root(baseUrl)}/releases/${expectedKey}`
      || typeof entry.filename !== 'string'
      || !/^[A-Za-z0-9._+-]+$/.test(entry.filename)
      || !Number.isSafeInteger(entry.bytes)
      || entry.bytes <= 0
      || typeof entry.sha256 !== 'string'
      || !/^[0-9a-f]{64}$/.test(entry.sha256)
    ) {
      throw new Error(`published app manifest at ${url} has invalid metadata for ${field}`)
    }
  }
  for (const [versioned, mutable] of [['dmg', 'latest_dmg'], ['zip', 'latest_zip']]) {
    const hasVersioned = latest[versioned] != null
    const hasMutable = latest[mutable] != null
    if (hasVersioned !== hasMutable) {
      throw new Error(
        `published app manifest at ${url} must contain ${versioned} and ${mutable} together`
      )
    }
    if (
      hasVersioned
      && (
        latest[versioned].bytes !== latest[mutable].bytes
        || latest[versioned].sha256 !== latest[mutable].sha256
      )
    ) {
      throw new Error(
        `published app manifest at ${url} has inconsistent ${versioned}/${mutable} artifacts`
      )
    }
  }
  return latest
}

export async function readAllPublishedAppLatest({
  fetchImpl = fetch,
  baseUrl,
  timeoutMs = 10_000
}) {
  const entries = await Promise.all(RELEASE_CHANNELS.map(async (channel) => [
    channel,
    await readPublishedAppLatest({ fetchImpl, baseUrl, channel, timeoutMs })
  ]))
  return Object.fromEntries(entries)
}

export async function findPublishedAppArtifacts({
  fetchImpl = fetch,
  baseUrl,
  channel,
  version,
  artifactKinds,
  timeoutMs = 10_000
}) {
  const checks = await Promise.all(artifactKinds.map(async (kind) => {
    const url = appVersionedArtifactUrl(baseUrl, channel, version, kind)
    const response = await fetchImpl(uncachedUrl(url), requestOptions('HEAD', timeoutMs))
    if (response.status === 404) return null
    if (!response.ok) throw new Error(`could not verify ${url}: HTTP ${response.status}`)
    return { kind, url }
  }))
  return checks.filter(Boolean)
}

export function planAppRelease({
  channel,
  version,
  build,
  artifactKinds,
  publishedByChannel,
  force = false
}) {
  const publishedLatest = publishedByChannel[channel] ?? null
  const sameVersion = publishedLatest?.version === version
  const fullImmutableSet = artifactKinds.includes('dmg') && artifactKinds.includes('zip')

  if (!sameVersion && !fullImmutableSet) {
    throw new Error(
      `Starting ${channel} ${version} requires both --dmg and --zip. ` +
        'A partial new-version publish would replace latest.json with incomplete download metadata.'
    )
  }

  const requestedBuild = positiveBuild(build)
  const effectiveBuild = requestedBuild ?? (sameVersion ? positiveBuild(publishedLatest.build) : null)
  if (!effectiveBuild) {
    throw new Error('--build must be a positive integer for a new app release')
  }

  if (sameVersion) {
    const publishedBuild = positiveBuild(publishedLatest.build)
    if (requestedBuild && requestedBuild !== publishedBuild && (!force || !fullImmutableSet)) {
      throw new Error(
        `${channel} ${version} is already build ${publishedBuild}; ` +
          `changing it to build ${requestedBuild} requires --force with both --dmg and --zip`
      )
    }
    return { build: force && requestedBuild ? requestedBuild : publishedBuild, publishedLatest }
  }

  if (!force) {
    for (const [publishedChannel, latest] of Object.entries(publishedByChannel)) {
      if (!latest) continue
      const publishedBuild = positiveBuild(latest.build)
      if (Number(effectiveBuild) <= Number(publishedBuild)) {
        throw new Error(
          `--build ${effectiveBuild} is not greater than build ${publishedBuild} already published on ` +
            `${publishedChannel}; CFBundleVersion is monotonic across channels`
        )
      }
    }
  }

  return { build: effectiveBuild, publishedLatest: null }
}

export function mergeAppLatest({
  channel,
  version,
  build,
  publishedAt,
  publishedLatest,
  newFields
}) {
  const preserved = {}
  if (publishedLatest?.version === version) {
    for (const field of APP_ARTIFACT_FIELDS) {
      if (publishedLatest[field] != null) preserved[field] = publishedLatest[field]
    }
  }
  return {
    channel,
    version,
    build,
    published_at: publishedAt,
    ...preserved,
    ...newFields
  }
}
