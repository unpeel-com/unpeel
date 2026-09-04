#!/usr/bin/env node
// Build and publish an Unpeel App's tarballs that back
// `curl -fsSL https://unpeel.com/install/<app>/install.sh | sh`.
//
// Operator script, same transport as release-cli.mjs (wrangler r2 object put
// into the unpeel-releases bucket). R2 key layout under the channel roots:
//   <channel>/<app>/unpeel-<app>-<version>-<target>.tar.gz   (immutable)
//   <channel>/<app>/unpeel-<app>-latest-<target>.tar.gz      (5-min cache)
//   <channel>/<app>/unpeel-<app>-latest-<target>.tar.gz.sha256
//
// Every app crate lives in the sibling repo ~/Dev/unpeel-app-<app> and names
// its binary unpeel-<app>. `protocol/app-registry.json` is the one
// serving/publishing allowlist; the Worker route itself is shared.
//
// Usage (from a Mac — builds the macos-universal tarball itself):
//   node scripts/release-app.mjs --app usage --channel beta [--dry-run]
// Linux tarballs are built elsewhere and attached:
//   node scripts/release-app.mjs --app usage --channel beta \
//     --linux-aarch64 path/to/unpeel-usage-linux-aarch64.tar.gz

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  assertPublishableAppReleaseSource,
  readReleaseSourceState
} from './release-source-state.mjs'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
// Publishing coordinates live in scripts/r2.jsonc (see release-cli.mjs).
const r2Config = readFileSync(resolve(repoRoot, 'scripts/r2.jsonc'), 'utf8')
const configAccountId = r2Config.match(/"account_id"\s*:\s*"([^"]+)"/)?.[1]
const configBucket = r2Config.match(/"bucket"\s*:\s*"([^"]+)"/)?.[1]

const args = {}
const argv = process.argv
for (let i = 2; i < argv.length; i += 1) {
  const arg = argv[i]
  if (!arg.startsWith('--')) throw new Error(`Unexpected argument: ${arg}`)
  const key = arg.slice(2)
  if (key === 'dry-run' || key === 'skip-build' || key === 'allow-dirty') {
    args[key] = true
  } else {
    args[key] = argv[++i]
  }
}

const registry = JSON.parse(
  readFileSync(resolve(repoRoot, 'protocol/app-registry.json'), 'utf8')
)
const KNOWN_APPS = Object.keys(registry)
const app = String(args.app ?? '')
if (!KNOWN_APPS.includes(app)) {
  throw new Error(`--app must be one of: ${KNOWN_APPS.join(', ')}`)
}
const bin = `unpeel-${app}`
const designDir = resolve(repoRoot, `../unpeel-app-${app}`)

const channel = String(args.channel ?? '')
if (!['alpha', 'beta', 'stable'].includes(channel)) {
  throw new Error('--channel must be alpha, beta, or stable')
}
const bucket = String(args.bucket ?? configBucket ?? 'unpeel-releases')
const dryRun = Boolean(args['dry-run'])
const allowDirty = Boolean(args['allow-dirty'])

// Never upload protocol/app-registry.json (or any App artifact) from a dirty
// or unaligned tree: a real publish reads the registry from the working tree,
// so an uncommitted edit would ship silently. Same gate release-cli.mjs uses.
const sourceState = readReleaseSourceState(repoRoot, { checkRemote: !dryRun })
assertPublishableAppReleaseSource(sourceState, { dryRun, allowDirty })

const manifest = readFileSync(resolve(designDir, 'Cargo.toml'), 'utf8')
const version = String(
  args.version ?? manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? ''
)
if (!version) throw new Error(`could not read version from unpeel-app-${app}/Cargo.toml`)

function run(command, commandArgs, options = {}) {
  console.log(`$ ${command} ${commandArgs.join(' ')}`)
  const result = spawnSync(command, commandArgs, { stdio: 'inherit', ...options })
  if (result.status !== 0) throw new Error(`${command} failed`)
}

// ---- Build the macos-universal tarball ------------------------------------

const tarballs = {} // target -> local tar.gz path
for (const target of ['linux-x86_64', 'linux-aarch64']) {
  if (args[target]) tarballs[target] = resolve(process.cwd(), String(args[target]))
}
if (!args['skip-build'] && process.platform === 'darwin') {
  const triples = ['aarch64-apple-darwin', 'x86_64-apple-darwin']
  for (const triple of triples) {
    run('cargo', ['build', '--release', '--target', triple], { cwd: designDir })
  }
  const stage = mkdtempSync(resolve(tmpdir(), `${bin}-`))
  const out = resolve(stage, bin)
  run('lipo', [
    '-create', '-output', out,
    ...triples.map((t) => resolve(designDir, 'target', t, 'release', bin))
  ])
  // lipo drops the arm64 slice's linker-generated signature; re-sign ad hoc.
  run('codesign', ['--force', '--sign', '-', out])
  const archive = resolve(stage, `${bin}-${version}-macos-universal.tar.gz`)
  run('tar', ['-czf', archive, '-C', stage, bin])
  tarballs['macos-universal'] = archive
}

if (Object.keys(tarballs).length === 0) {
  throw new Error('nothing to publish: no tarballs built or attached')
}

// ---- Publish --------------------------------------------------------------

const artifactCache = 'public, max-age=31536000, immutable'
const downloadCache = 'public, max-age=300, must-revalidate'

function wranglerPut(file, key, cacheControl) {
  const contentType = key.endsWith('.sha256') ? 'text/plain'
    : key.endsWith('.json') ? 'application/json; charset=utf-8'
    : 'application/gzip'
  const wranglerArgs = [
    'wrangler', 'r2', 'object', 'put',
    `${bucket}/${key}`,
    '--file', file,
    '--content-type', contentType,
    '--cache-control', cacheControl,
    '--remote'
  ]
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

for (const [target, file] of Object.entries(tarballs)) {
  const digest = createHash('sha256').update(readFileSync(file)).digest('hex')
  const sidecar = `${file}.sha256`
  writeFileSync(sidecar, `${digest}  ${bin}-${version}-${target}.tar.gz\n`)
  const prefix = `${channel}/${app}`
  wranglerPut(file, `${prefix}/${bin}-${version}-${target}.tar.gz`, artifactCache)
  wranglerPut(file, `${prefix}/${bin}-latest-${target}.tar.gz`, downloadCache)
  wranglerPut(sidecar, `${prefix}/${bin}-latest-${target}.tar.gz.sha256`, downloadCache)
  console.log(
    `${target}: ${(statSync(file).size / 1024 / 1024).toFixed(1)} MiB, sha256 ${digest.slice(0, 12)}…`
  )
}

// Publishing an App also republishes the registry the Worker serves the
// /install/<app>/install.sh route from (release:cli does the same).
wranglerPut(
  resolve(repoRoot, 'protocol/app-registry.json'),
  `${channel}/protocol/app-registry.json`,
  downloadCache
)

console.log(
  `${dryRun ? '[dry-run] ' : ''}published ${bin} ${version} to ${channel}/${app}/`
)
