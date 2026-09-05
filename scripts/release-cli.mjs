#!/usr/bin/env node
// Build and publish the Unpeel CLI (`unpeel` + `unpeel-host` + `unpeel-attach`) tarballs that
// back `curl -fsSL https://unpeel.com/install.sh | sh`.
//
// Operator/CI script, same transport as publish-cloudflare-release.mjs
// (wrangler r2 object put into the unpeel-releases bucket). R2 key layout,
// under the same channel roots the app releases use:
//   <channel>/cli/unpeel-<version>-<target>.tar.gz         (immutable)
//   <channel>/cli/unpeel-<version>-<revision>-<target>.tar.gz(.sha256)
//                                                         (immutable recovery)
//   <channel>/cli/unpeel-latest-<target>.tar.gz            (5-min cache)
//   <channel>/cli/unpeel-latest-<target>.tar.gz.sha256     (integrity sidecar)
//   <channel>/cli/latest.json                              (60-s manifest)
//
// Usage (from a Mac — builds the macos-universal tarball itself):
//   node scripts/release-cli.mjs --channel beta [--version 0.1.0] [--dry-run]
// Linux tarballs are built elsewhere (a Linux box or CI) and attached:
//   node scripts/release-cli.mjs --channel beta \
//     --linux-x86_64 path/to/unpeel-linux-x86_64.tar.gz \
//     --linux-aarch64 path/to/unpeel-linux-aarch64.tar.gz
// Targets not provided are left untouched in the bucket (a Linux-only publish
// does not disturb the macOS artifacts, and vice versa). Same-version staged
// publishes merge their target entries into latest.json. The first publish of
// a version requires all three supported targets so latest.json is never
// replaced with a temporarily incomplete release.

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  copyFileSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync
} from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  assertCliArchiveEntries,
  assertCliArtifactRevisionPublish,
  assertSafeCliTargetSet,
  CLI_ARCHIVE_GENERATED_DIR,
  CLI_ARCHIVE_PROTOCOL_DIR,
  CLI_BINARIES,
  cliVersionedArtifactKey,
  findPublishedCliArtifacts,
  isCompleteCliTargetSet,
  mergeCliLatest,
  readPublishedCliLatest,
  validateCliArtifactRevision
} from './release-cli-state.mjs'
import {
  assertPublishableReleaseSource,
  cliBuildProvenance,
  readReleaseSourceState,
  validateCliBinaryTarget,
  validateCliBuildProvenance
} from './release-source-state.mjs'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const cratesDir = resolve(repoRoot, 'crates')
const attachDir = resolve(repoRoot, 'crates', 'unpeel-attach')
const protocolDir = resolve(repoRoot, CLI_ARCHIVE_PROTOCOL_DIR)
const generatedDir = resolve(repoRoot, CLI_ARCHIVE_GENERATED_DIR)
// Publishing coordinates live in scripts/r2.jsonc (no dependency on the
// website package or the release Worker; wrangler is a root devDependency).
const r2Config = readFileSync(resolve(repoRoot, 'scripts/r2.jsonc'), 'utf8')
const configAccountId = r2Config.match(/"account_id"\s*:\s*"([^"]+)"/)?.[1]
const configBucket = r2Config.match(/"bucket"\s*:\s*"([^"]+)"/)?.[1]

function parseArgs(argv) {
  const allowed = new Set([
    'channel', 'version', 'bucket', 'base-url', 'dry-run', 'force',
    'artifact-revision', 'skip-build', 'macos-universal',
    'linux-x86_64', 'linux-aarch64'
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
const bucket = String(args.bucket ?? process.env.UNPEEL_RELEASE_BUCKET ?? configBucket ?? 'unpeel-releases')
const baseUrl = String(args['base-url'] ?? process.env.UNPEEL_RELEASE_BASE_URL ?? 'https://unpeel.com')
const dryRun = Boolean(args['dry-run'])
const force = Boolean(args.force)
const skipBuild = Boolean(args['skip-build'])

if (!['alpha', 'beta', 'stable'].includes(channel)) {
  throw new Error(`--channel must be alpha, beta, or stable, got ${channel}`)
}

const workspaceToml = readFileSync(resolve(cratesDir, 'Cargo.toml'), 'utf8')
const workspaceVersion = workspaceToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1]
const version = String(args.version ?? workspaceVersion ?? '')
if (!version) throw new Error('--version is required (workspace version not found)')
if (!/^[A-Za-z0-9._-]+$/.test(version)) {
  throw new Error(`--version may only contain [A-Za-z0-9._-], got ${version}`)
}
// Lockstep versioning: the CLI and the app share the crates workspace version
// (the app side asserts the same in clients/native/release.sh). Bump
// crates/Cargo.toml to release a new version; an explicit --version may only
// restate it.
if (args.version != null && workspaceVersion && version !== workspaceVersion) {
  throw new Error(
    `--version ${version} does not match the crates workspace version ${workspaceVersion} — ` +
      `the app and CLI are versioned in lockstep; bump crates/Cargo.toml instead`
  )
}

const sourceState = readReleaseSourceState(repoRoot, { checkRemote: !dryRun })
if (!dryRun) assertPublishableReleaseSource(sourceState)
const artifactRevision = validateCliArtifactRevision(args['artifact-revision'], sourceState.head)
if (artifactRevision != null && force) {
  throw new Error('--artifact-revision cannot be combined with --force; revisioned keys are immutable')
}

function run(cmd, cmdArgs, opts = {}) {
  console.log([cmd, ...cmdArgs].join(' '))
  const result = spawnSync(cmd, cmdArgs, { stdio: 'inherit', ...opts })
  if (result.status !== 0) throw new Error(`${cmd} ${cmdArgs[0] ?? ''} failed`)
}

function rustReleaseEnvironment() {
  const sysroot = spawnSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' })
  if (sysroot.status !== 0 || !sysroot.stdout.trim()) {
    throw new Error('rustc --print sysroot failed while preparing deterministic release paths')
  }
  const cargoHome = process.env.CARGO_HOME ?? resolve(homedir(), '.cargo')
  const remapFlags = [
    `--remap-path-prefix=${repoRoot}=/unpeel/source`,
    `--remap-path-prefix=${cargoHome}=/cargo`,
    `--remap-path-prefix=${sysroot.stdout.trim()}=/rust/toolchain`
  ]
  const env = { ...process.env }
  if (Object.hasOwn(process.env, 'CARGO_ENCODED_RUSTFLAGS')) {
    env.CARGO_ENCODED_RUSTFLAGS = [process.env.CARGO_ENCODED_RUSTFLAGS, ...remapFlags]
      .filter(Boolean)
      .join('\x1f')
  } else {
    env.RUSTFLAGS = [process.env.RUSTFLAGS, ...remapFlags].filter(Boolean).join(' ')
  }
  return env
}

// ---- Build the macos-universal tarball (unless attached or skipped) --------

const tarballs = {} // target -> local tar.gz path
if (args['macos-universal']) {
  tarballs['macos-universal'] = resolve(repoRoot, String(args['macos-universal']))
} else if (!skipBuild && process.platform === 'darwin') {
  const triples = ['aarch64-apple-darwin', 'x86_64-apple-darwin']
  const rustEnv = rustReleaseEnvironment()
  for (const triple of triples) {
    run('cargo', [
      'build', '--release', '--locked',
      '--manifest-path', resolve(cratesDir, 'Cargo.toml'),
      '-p', 'unpeel-cli', '-p', 'unpeel-host',
      '--target', triple
    ], { env: rustEnv })
    // unpeel-attach is a standalone crate (its own [workspace]; never a
    // crates/ member), so it builds from its own manifest and target dir.
    run('cargo', [
      'build', '--release', '--locked',
      '--manifest-path', resolve(attachDir, 'Cargo.toml'),
      '--target', triple
    ], { env: rustEnv })
  }
  const stage = mkdtempSync(resolve(tmpdir(), 'unpeel-cli-'))
  for (const bin of CLI_BINARIES) {
    const out = resolve(stage, bin)
    const targetRoot = bin === 'unpeel-attach' ? attachDir : cratesDir
    run('lipo', [
      '-create', '-output', out,
      ...triples.map((t) => resolve(targetRoot, 'target', t, 'release', bin))
    ])
    // lipo drops the arm64 slice's linker-generated signature; re-sign ad hoc
    // so the binary runs on Apple silicon.
    run('codesign', ['--force', '--sign', '-', out])
  }
  copyFileSync(resolve(repoRoot, 'LICENSE'), resolve(stage, 'LICENSE'))
  run('cargo', [
    'run', '--quiet', '--locked',
    '--manifest-path', resolve(cratesDir, 'Cargo.toml'),
    '-p', 'unpeel-license-notices', '--',
    '--manifest-path', resolve(cratesDir, 'Cargo.toml'),
    '--package', 'unpeel-cli',
    '--package', 'unpeel-host',
    '--manifest-path', resolve(attachDir, 'Cargo.toml'),
    '--package', 'unpeel-attach',
    ...triples.flatMap((triple) => ['--target', triple]),
    '--output', resolve(stage, 'THIRD_PARTY_NOTICES.txt')
  ], { env: rustEnv })
  writeFileSync(
    resolve(stage, 'BUILD_PROVENANCE.json'),
    `${JSON.stringify(cliBuildProvenance({
      state: sourceState,
      version,
      target: 'macos-universal'
    }), null, 2)}\n`
  )
  // protocol/ rides along verbatim (decision 3, the private "repo-split-inventory" design record §7).
  cpSync(protocolDir, resolve(stage, CLI_ARCHIVE_PROTOCOL_DIR), { recursive: true })
  // generated/ (the client-safe runtime catalog) rides along the same way.
  cpSync(generatedDir, resolve(stage, CLI_ARCHIVE_GENERATED_DIR), { recursive: true })
  const tarPath = resolve(stage, `unpeel-${version}-macos-universal.tar.gz`)
  run('tar', [
    '-czf', tarPath, '-C', stage,
    ...CLI_BINARIES, 'LICENSE', 'THIRD_PARTY_NOTICES.txt', 'BUILD_PROVENANCE.json',
    CLI_ARCHIVE_PROTOCOL_DIR, CLI_ARCHIVE_GENERATED_DIR
  ])
  tarballs['macos-universal'] = tarPath
} else if (!skipBuild) {
  console.warn('warning: not on macOS — skipping the macos-universal build (attach it with --macos-universal)')
}

for (const target of ['linux-x86_64', 'linux-aarch64']) {
  if (args[target]) tarballs[target] = resolve(repoRoot, String(args[target]))
}

const targets = Object.keys(tarballs)
if (targets.length === 0) throw new Error('Nothing to publish: no tarball built or attached')
for (const [target, file] of Object.entries(tarballs)) {
  if (!existsSync(file)) throw new Error(`Tarball not found for ${target}: ${file}`)
  const listing = spawnSync('tar', ['-tzf', file], { encoding: 'utf8' })
  if (listing.status !== 0) throw new Error(`Could not inspect tarball for ${target}: ${file}`)
  const entries = listing.stdout
    .split('\n')
    .filter(Boolean)
    .map((entry) => entry.replace(/^(\.\/)+/, '').replace(/\/$/, ''))
  const unsafe = entries.find((entry) => entry.startsWith('/') || entry.split('/').includes('..'))
  if (unsafe) throw new Error(`Tarball for ${target} contains an unsafe path: ${unsafe}`)
  assertCliArchiveEntries(entries, target)
  const inspectDir = mkdtempSync(resolve(tmpdir(), 'unpeel-cli-inspect-'))
  try {
    const extraction = spawnSync('tar', [
      '-xzf', file, '-C', inspectDir,
      ...CLI_BINARIES, 'BUILD_PROVENANCE.json'
    ], { encoding: 'utf8' })
    if (extraction.status !== 0) {
      throw new Error(`Could not inspect release payloads for ${target}: ${file}`)
    }
    for (const binary of CLI_BINARIES) {
      const binaryPath = resolve(inspectDir, binary)
      const binaryStat = lstatSync(binaryPath)
      if (!binaryStat.isFile() || (binaryStat.mode & 0o111) === 0) {
        throw new Error(`${binary} in ${target} is not a regular executable file`)
      }
      validateCliBinaryTarget({
        header: readFileSync(binaryPath).subarray(0, 4096),
        target,
        binary
      })
    }
    const provenancePath = resolve(inspectDir, 'BUILD_PROVENANCE.json')
    if (!lstatSync(provenancePath).isFile()) {
      throw new Error(`BUILD_PROVENANCE.json in ${target} is not a regular file`)
    }
    let provenance
    try {
      provenance = JSON.parse(readFileSync(provenancePath, 'utf8'))
    } catch (error) {
      throw new Error(`Invalid BUILD_PROVENANCE.json for ${target}: ${error.message}`)
    }
    validateCliBuildProvenance({
      provenance,
      version,
      target,
      sourceState,
      publishing: !dryRun
    })
  } finally {
    rmSync(inspectDir, { recursive: true, force: true })
  }
}

// ---- Publish ---------------------------------------------------------------

const artifactCache = 'public, max-age=31536000, immutable'
const manifestCache = 'public, max-age=60, must-revalidate'
const downloadCache = 'public, max-age=300, must-revalidate'

function fileInfo(file, key, filename = basename(file)) {
  const body = readFileSync(file)
  return {
    key,
    path: `/releases/${key}`,
    url: `${baseUrl.replace(/\/$/, '')}/releases/${key}`,
    filename,
    bytes: statSync(file).size,
    sha256: createHash('sha256').update(body).digest('hex')
  }
}

function wranglerPut(file, key, contentType, cacheControl, filename) {
  const wranglerArgs = [
    'wrangler', 'r2', 'object', 'put',
    `${bucket}/${key}`,
    '--file', file,
    '--content-type', contentType,
    '--cache-control', cacheControl,
    '--remote'
  ]
  if (filename) {
    wranglerArgs.push('--content-disposition', `attachment; filename="${filename}"`)
  }
  console.log(`npx ${wranglerArgs.map((part) => JSON.stringify(part)).join(' ')}`)
  if (dryRun) return
  const result = spawnSync('npx', wranglerArgs, {
    cwd: repoRoot,
    env: {
      ...process.env,
      ...(configAccountId ? { CLOUDFLARE_ACCOUNT_ID: configAccountId } : {})
    },
    stdio: 'inherit'
  })
  if (result.status !== 0) throw new Error(`wrangler upload failed for ${key}`)
}

// Versioned tarballs are immutable at the CDN. Check the versioned URLs
// themselves: latest.json may be incomplete after a historical partial
// publish, so trusting only its target list can silently overwrite an object
// that clients cache for a year. Also retain the manifest so a safe staged
// publish can merge previously published targets of this same version.
let publishedLatest = null
if (!dryRun) {
  try {
    publishedLatest = await readPublishedCliLatest({ baseUrl, channel })
  } catch (err) {
    if (!force || !isCompleteCliTargetSet(targets)) {
      throw new Error(
        `Could not safely read the published CLI manifest (${err?.message ?? err}). ` +
          'Refusing to publish because that could erase existing target entries. Retry when the endpoint is healthy; ' +
          '--force can replace unread state only when all three target archives are supplied.'
      )
    }
    console.warn(
      `warning: could not read the published CLI manifest (${err?.message ?? err}); ` +
        '--force permits a complete three-target replacement'
    )
  }

  assertCliArtifactRevisionPublish({
    artifactRevision,
    force,
    targets,
    version,
    publishedLatest,
    publishing: true
  })
  assertSafeCliTargetSet({ version, targets, publishedLatest, artifactRevision })

  if (!force) {
    let existing
    try {
      existing = await findPublishedCliArtifacts({
        baseUrl,
        channel,
        version,
        targets,
        artifactRevision
      })
    } catch (err) {
      throw new Error(
        `Could not verify immutable CLI artifact keys (${err?.message ?? err}). ` +
          'Refusing to publish; retry when the release endpoint is reachable.'
      )
    }
    if (existing.length > 0) {
      throw new Error(
        `${channel} cli ${version}${artifactRevision ? ` revision ${artifactRevision}` : ''} ` +
          `already has immutable artifacts for ${existing
            .map(({ target, kind }) => `${target}${kind ? ` ${kind}` : ''}`)
            .join(', ')}. ` +
          (artifactRevision
            ? 'Choose a new current-HEAD artifact revision.'
            : 'Bump the version, or pass --force to overwrite (danger: immutable CDN caches).')
      )
    }
  }
} else {
  assertCliArtifactRevisionPublish({
    artifactRevision,
    force,
    targets,
    version,
    publishedLatest: null,
    publishing: false
  })
}

const tmp = mkdtempSync(resolve(tmpdir(), 'unpeel-cli-publish-'))
const newTargetEntries = {}
const uploadEntries = []
for (const [target, file] of Object.entries(tarballs)) {
  const versionedKey = cliVersionedArtifactKey(
    channel,
    version,
    target,
    artifactRevision
  )
  const latestKey = `${channel}/cli/unpeel-latest-${target}.tar.gz`
  const versionedFilename = basename(versionedKey)
  const info = fileInfo(
    file,
    versionedKey,
    artifactRevision == null ? basename(file) : versionedFilename
  )
  // The versioned key always gets an immutable `.sha256` sidecar: a Mac
  // app build that bundles a published archive (`clients/native/build-app.sh`
  // with UNPEEL_SERVER_ARCHIVE) verifies the exact versioned archive
  // against this sidecar, never against the mutable `-latest`.
  const versionedShaKey = `${versionedKey}.sha256`
  const versionedShaPath = resolve(tmp, `${target}-versioned.sha256`)
  const latestShaPath = resolve(tmp, `${target}-latest.sha256`)
  writeFileSync(versionedShaPath, `${info.sha256}  ${versionedFilename}\n`)
  writeFileSync(latestShaPath, `${info.sha256}  ${basename(latestKey)}\n`)

  uploadEntries.push({
    target,
    file,
    versionedKey,
    versionedFilename,
    versionedShaKey,
    versionedShaPath,
    latestKey,
    latestShaPath
  })
  newTargetEntries[target] = {
    ...info,
    ...(artifactRevision == null
      ? {}
      : {
          sidecar_key: versionedShaKey,
          sidecar_path: `/releases/${versionedShaKey}`,
          sidecar_url: `${baseUrl.replace(/\/$/, '')}/releases/${versionedShaKey}`
        }),
    latest_key: latestKey,
    latest_path: `/releases/${latestKey}`
  }
}

// Recovery safety: finish every new immutable object before touching any
// mutable alias. A failed immutable phase therefore leaves every installer on
// the previously coherent latest archive + checksum pair.
for (const entry of uploadEntries) {
  wranglerPut(
    entry.file,
    entry.versionedKey,
    'application/gzip',
    artifactCache,
    entry.versionedFilename
  )
  wranglerPut(
    entry.versionedShaPath,
    entry.versionedShaKey,
    'text/plain; charset=utf-8',
    artifactCache
  )
}

for (const entry of uploadEntries) {
  wranglerPut(
    entry.file,
    entry.latestKey,
    'application/gzip',
    downloadCache,
    `unpeel-${entry.target}.tar.gz`
  )
  wranglerPut(
    entry.latestShaPath,
    `${entry.latestKey}.sha256`,
    'text/plain; charset=utf-8',
    downloadCache
  )
}

const latest = mergeCliLatest({
  channel,
  version,
  artifactRevision,
  publishedAt: new Date().toISOString(),
  publishedLatest,
  newTargets: newTargetEntries
})
const latestPath = resolve(tmp, 'latest.json')
writeFileSync(latestPath, `${JSON.stringify(latest, null, 2)}\n`)
wranglerPut(latestPath, `${channel}/cli/latest.json`, 'application/json; charset=utf-8', manifestCache)

// The installer scripts and the App registry are published per channel too:
// the release Worker (website repo) fetches them from R2 per request and only
// falls back to its deploy-time copy when the channel has none. Placeholders
// (__DEFAULT_CHANNEL__, __BASE_URL__, __APP__ …) stay in the uploaded text;
// the Worker substitutes them per request as before.
for (const [file, key, contentType] of [
  ['scripts/install.sh', `${channel}/cli/install.sh`, 'text/x-shellscript; charset=utf-8'],
  ['scripts/install-app.sh', `${channel}/cli/install-app.sh`, 'text/x-shellscript; charset=utf-8'],
  ['protocol/app-registry.json', `${channel}/protocol/app-registry.json`, 'application/json; charset=utf-8']
]) {
  wranglerPut(resolve(repoRoot, file), key, contentType, manifestCache)
}

console.log(
  `Published cli ${channel} ${version}` +
    `${artifactRevision ? ` revision ${artifactRevision}` : ''} (${targets.join(', ')}) to ${bucket}`
)
console.log('Install with: curl -fsSL https://unpeel.com/install.sh | sh')
