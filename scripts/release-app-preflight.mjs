#!/usr/bin/env node

import {
  findPublishedAppArtifacts,
  planAppRelease,
  readAllPublishedAppLatest
} from './release-app-state.mjs'

function parseArgs(argv) {
  const out = {}
  for (let index = 2; index < argv.length; index += 1) {
    const arg = argv[index]
    if (!arg.startsWith('--')) throw new Error(`Unexpected argument: ${arg}`)
    const next = argv[index + 1]
    if (!next || next.startsWith('--')) {
      out[arg.slice(2)] = true
    } else {
      out[arg.slice(2)] = next
      index += 1
    }
  }
  return out
}

const args = parseArgs(process.argv)
const baseUrl = String(args['base-url'] ?? 'https://unpeel.com')
const channel = String(args.channel ?? '')
const version = String(args.version ?? '')
const build = args.build == null ? undefined : String(args.build)

if (!['alpha', 'beta', 'stable'].includes(channel)) throw new Error(`invalid --channel: ${channel}`)
if (!version) throw new Error('--version is required')
if (!/^[A-Za-z0-9._-]+$/.test(version)) {
  throw new Error(`--version may only contain [A-Za-z0-9._-], got ${version}`)
}

const publishedByChannel = await readAllPublishedAppLatest({ baseUrl })
planAppRelease({
  channel,
  version,
  build,
  artifactKinds: ['dmg', 'zip'],
  publishedByChannel
})

const existing = await findPublishedAppArtifacts({
  baseUrl,
  channel,
  version,
  artifactKinds: ['dmg', 'zip']
})
if (existing.length > 0) {
  throw new Error(
    `${channel} ${version} already has immutable ${existing.map(({ kind }) => kind).join(', ')} artifacts. ` +
      'Bump the version, or use --force only for an intentional recovery.'
  )
}

console.log(`Release preflight passed for ${channel} ${version} (build ${build})`)
