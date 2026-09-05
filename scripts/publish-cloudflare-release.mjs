#!/usr/bin/env node
// Publish Unpeel release artifacts to Cloudflare R2 through Wrangler.
//
// This is intentionally an operator/CI script, not a public admin endpoint.
// Usage:
//   node scripts/publish-cloudflare-release.mjs \
//     --channel beta \
//     --version 0.1.0-beta.1 \
//     --build 3 \
//     --dmg apps/native/dist/Unpeel.dmg \
//     --zip apps/native/dist/Unpeel-0.1.0-beta.1.zip \
//     --appcast apps/native/dist/appcast-beta.xml

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdtempSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  findPublishedAppArtifacts,
  mergeAppLatest,
  planAppRelease,
  readAllPublishedAppLatest
} from './release-app-state.mjs'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
// Publishing coordinates live in scripts/r2.jsonc (account + bucket only);
// wrangler runs from the repo root with the operator's own login. The app
// repo carries no Worker: that is deployed from the website repo.
const r2Config = readFileSync(resolve(repoRoot, 'scripts/r2.jsonc'), 'utf8')
const configAccountId = r2Config.match(/"account_id"\s*:\s*"([^"]+)"/)?.[1]
const configBucket = r2Config.match(/"bucket"\s*:\s*"([^"]+)"/)?.[1]

function parseArgs(argv) {
  const allowed = new Set([
    'channel', 'version', 'build', 'bucket', 'base-url', 'dry-run', 'force',
    'dmg', 'zip', 'appcast'
  ])
  const out = {}
  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i]
    if (!arg.startsWith('--')) throw new Error(`Unexpected argument: ${arg}`)
    if (arg.includes('=')) {
      throw new Error(`Unexpected argument form: ${arg} (pass values as a separate argument)`)
    }
    const key = arg.slice(2)
    if (!allowed.has(key)) throw new Error(`Unknown option: --${key}`)
    const next = argv[i + 1]
    if (!next || next.startsWith('--')) {
      out[key] = true
    } else {
      out[key] = next
      i += 1
    }
  }
  return out
}

const args = parseArgs(process.argv)
const channel = String(args.channel ?? 'beta').toLowerCase()
const version = String(args.version ?? '')
const build = args.build == null ? undefined : String(args.build)
const bucket = String(args.bucket ?? process.env.UNPEEL_RELEASE_BUCKET ?? configBucket ?? 'unpeel-releases')
const baseUrl = String(args['base-url'] ?? process.env.UNPEEL_RELEASE_BASE_URL ?? 'https://unpeel.com')
const dryRun = Boolean(args['dry-run'])
const force = Boolean(args.force)

if (!['alpha', 'beta', 'stable'].includes(channel)) {
  throw new Error(`--channel must be alpha, beta, or stable, got ${channel}`)
}
if (!version) throw new Error('--version is required')
// The version becomes an R2 object key and a public URL path segment. The
// serving workers only accept [A-Za-z0-9._/@+-], and a literal '+' would be
// percent-encoded by clients and 400 — so reject anything outside the safe set
// here, at publish time, instead of uploading an unservable artifact.
if (!/^[A-Za-z0-9._-]+$/.test(version)) {
  throw new Error(`--version may only contain [A-Za-z0-9._-], got ${version}`)
}

const dmg = args.dmg ? resolve(repoRoot, String(args.dmg)) : null
const zip = args.zip ? resolve(repoRoot, String(args.zip)) : null
const appcast = args.appcast ? resolve(repoRoot, String(args.appcast)) : null
if (!dmg && !zip && !appcast) throw new Error('Provide at least one of --dmg, --zip, or --appcast')
if (build != null && !/^[1-9][0-9]*$/.test(build)) {
  throw new Error(`--build must be a positive integer, got ${build}`)
}

for (const file of [dmg, zip, appcast].filter(Boolean)) {
  if (!existsSync(file)) throw new Error(`File not found: ${file}`)
}

const artifactCache = 'public, max-age=31536000, immutable'
const manifestCache = 'public, max-age=60, must-revalidate'
const downloadCache = 'public, max-age=300, must-revalidate'

function fileInfo(file, key) {
  const body = readFileSync(file)
  return {
    key,
    path: `/releases/${key}`,
    url: `${baseUrl.replace(/\/$/, '')}/releases/${key}`,
    filename: basename(file),
    bytes: statSync(file).size,
    sha256: createHash('sha256').update(body).digest('hex')
  }
}

function wranglerPut(file, key, contentType, cacheControl, filename) {
  const cmd = 'npx'
  const wranglerArgs = [
    'wrangler',
    'r2',
    'object',
    'put',
    `${bucket}/${key}`,
    '--file',
    file,
    '--content-type',
    contentType,
    '--cache-control',
    cacheControl,
    '--remote'
  ]
  if (filename) {
    wranglerArgs.push('--content-disposition', `attachment; filename="${filename}"`)
  }

  const rendered = `${cmd} ${wranglerArgs.map((part) => JSON.stringify(part)).join(' ')}`
  console.log(dryRun ? `[dry-run] ${rendered}` : rendered)
  if (dryRun) return

  const result = spawnSync(cmd, wranglerArgs, {
    cwd: repoRoot,
    env: {
      ...process.env,
      ...(configAccountId ? { CLOUDFLARE_ACCOUNT_ID: configAccountId } : {})
    },
    stdio: 'inherit'
  })
  if (result.status !== 0) {
    throw new Error(`wrangler upload failed for ${key}`)
  }
}

// Read and validate every channel so CFBundleVersion stays globally monotonic.
// Directly HEAD the versioned DMG/ZIP keys too: latest.json may be incomplete
// after a failed historical publish, and those objects are cached immutable
// for a year. Same-version partial operations merge the existing manifest;
// starting a new version requires both immutable download artifacts.
const artifactKinds = [dmg ? 'dmg' : null, zip ? 'zip' : null].filter(Boolean)
let publishedLatest = null
let effectiveBuild = build
if (!dryRun) {
  let publishedByChannel
  try {
    publishedByChannel = await readAllPublishedAppLatest({ baseUrl })
  } catch (err) {
    const fullImmutableSet = artifactKinds.includes('dmg') && artifactKinds.includes('zip')
    if (!force || !fullImmutableSet) {
      throw new Error(
        `Could not safely read the published app manifests (${err?.message ?? err}). ` +
          'Refusing to publish: a partial operation cannot reconstruct latest.json, and a full operation requires --force for intentional recovery.'
      )
    }
    console.warn(`warning: could not read published app manifests (${err?.message ?? err}); --force permits a full replacement`)
    publishedByChannel = { alpha: null, beta: null, stable: null }
  }

  const plan = planAppRelease({
    channel,
    version,
    build,
    artifactKinds,
    publishedByChannel,
    force
  })
  publishedLatest = plan.publishedLatest
  effectiveBuild = plan.build

  if (!force) {
    let existing
    try {
      existing = await findPublishedAppArtifacts({ baseUrl, channel, version, artifactKinds })
    } catch (err) {
      throw new Error(
        `Could not verify immutable app artifact keys (${err?.message ?? err}). ` +
          'Refusing to publish; retry when the release endpoint is reachable.'
      )
    }
    if (existing.length > 0) {
      throw new Error(
        `${channel} ${version} already has immutable ${existing.map(({ kind }) => kind).join(', ')} artifacts. ` +
          'Bump the version, or pass --force to overwrite (danger: immutable CDN caches).'
      )
    }
  }
}

const newFields = {}

if (dmg) {
  const versionedKey = `${channel}/Unpeel-${version}.dmg`
  const latestKey = `${channel}/Unpeel-latest.dmg`
  wranglerPut(dmg, versionedKey, 'application/x-apple-diskimage', artifactCache, `Unpeel-${version}.dmg`)
  wranglerPut(dmg, latestKey, 'application/x-apple-diskimage', downloadCache, 'Unpeel-latest.dmg')
  newFields.dmg = fileInfo(dmg, versionedKey)
  newFields.latest_dmg = fileInfo(dmg, latestKey)
}

if (zip) {
  const versionedKey = `${channel}/Unpeel-${version}.zip`
  const latestKey = `${channel}/Unpeel-latest.zip`
  wranglerPut(zip, versionedKey, 'application/zip', artifactCache, `Unpeel-${version}.zip`)
  wranglerPut(zip, latestKey, 'application/zip', downloadCache, 'Unpeel-latest.zip')
  newFields.zip = fileInfo(zip, versionedKey)
  newFields.latest_zip = fileInfo(zip, latestKey)
}

if (appcast) {
  const key = `${channel}/appcast.xml`
  wranglerPut(appcast, key, 'application/xml; charset=utf-8', manifestCache)
  newFields.appcast = fileInfo(appcast, key)
}

const tmp = mkdtempSync(resolve(tmpdir(), 'unpeel-release-'))
const latest = mergeAppLatest({
  channel,
  version,
  build: effectiveBuild,
  publishedAt: new Date().toISOString(),
  publishedLatest,
  newFields
})
const latestPath = resolve(tmp, 'latest.json')
writeFileSync(latestPath, `${JSON.stringify(latest, null, 2)}\n`)
wranglerPut(latestPath, `${channel}/latest.json`, 'application/json; charset=utf-8', manifestCache)

if (dryRun) {
  console.log(`Dry run complete: would publish ${channel} ${version} metadata to ${bucket}; no R2 objects uploaded`)
} else {
  console.log(`Published ${channel} ${version} metadata to ${bucket}`)
}
