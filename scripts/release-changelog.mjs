#!/usr/bin/env node
// release-changelog.mjs — where the app release reads the website changelog.
//
// The changelog is the website's `/changelog` page and stays with the website
// (unpeel-website, a separate repository). The app cut reads it, in order of
// precedence:
//   1. UNPEEL_CHANGELOG=<path>            explicit override
//   2. ../unpeel-website/app/changelog.md the website sibling checkout
//   3. apps/website/app/changelog.md      a website checkout inside this tree
// and fails with a message naming the sibling checkout when none exists.
//
// Usage (from release.sh): node scripts/release-changelog.mjs [--repo-root DIR]
// Prints the resolved path on stdout; exits 1 with the reason on stderr.

import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const CHANGELOG_CANDIDATES = Object.freeze([
  // The website repo keeps the monorepo layout (apps/website/app/…).
  ['website sibling', ['..', 'unpeel-website', 'apps', 'website', 'app', 'changelog.md']],
  ['website sibling', ['..', 'unpeel-website', 'app', 'changelog.md']],
  ['monorepo', ['apps', 'website', 'app', 'changelog.md']]
])

/**
 * @param {{ repoRoot: string, env?: NodeJS.ProcessEnv, exists?: (path: string) => boolean }} options
 * @returns {{ path: string, source: 'override' | 'website sibling' | 'monorepo' }}
 */
export function resolveChangelogPath({ repoRoot, env = process.env, exists = existsSync }) {
  const override = env.UNPEEL_CHANGELOG?.trim()
  if (override) {
    const path = resolve(repoRoot, override)
    if (!exists(path)) {
      throw new Error(`UNPEEL_CHANGELOG points at a missing file: ${path}`)
    }
    return { path, source: 'override' }
  }
  for (const [source, segments] of CHANGELOG_CANDIDATES) {
    const path = resolve(repoRoot, ...segments)
    if (exists(path)) return { path, source }
  }
  throw new Error(
    'no website changelog found. The app release reads the website\'s changelog.md ' +
      `(${CHANGELOG_CANDIDATES.map(([, segments]) => segments.join('/')).join(' or ')} ` +
      `relative to ${repoRoot}). Clone unpeel-website next to this repo, or set UNPEEL_CHANGELOG.`
  )
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2)
  let repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--repo-root' && argv[i + 1]) {
      repoRoot = resolve(argv[i + 1])
      i += 1
    } else {
      console.error(`Unknown argument: ${argv[i]}`)
      process.exit(2)
    }
  }
  try {
    process.stdout.write(`${resolveChangelogPath({ repoRoot }).path}\n`)
  } catch (error) {
    console.error(`error: ${error.message}`)
    process.exit(1)
  }
}
