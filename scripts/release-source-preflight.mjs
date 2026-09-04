#!/usr/bin/env node

import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  assertPublishableReleaseSource,
  readReleaseSourceState
} from './release-source-state.mjs'

const repoRoot = resolve(fileURLToPath(new URL('..', import.meta.url)))
const state = readReleaseSourceState(repoRoot, { checkRemote: true })
assertPublishableReleaseSource(state)
console.log(`Release source preflight passed: main at ${state.head}`)
