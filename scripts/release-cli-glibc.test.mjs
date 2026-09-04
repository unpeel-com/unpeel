import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { resolve } from 'node:path'
import test from 'node:test'

const helper = resolve('scripts/cli-glibc.sh')

function versionAtMost(actual, ceiling) {
  return spawnSync(
    'sh',
    ['-c', '. "$1"; unpeel_glibc_version_at_most "$2" "$3"', 'sh', helper, actual, ceiling],
    { encoding: 'utf8' }
  )
}

test('GLIBC ceiling accepts older, equal, and shorter equivalent versions', () => {
  for (const actual of ['2.17', '2.29', '2.31', '2.31.0']) {
    assert.equal(versionAtMost(actual, '2.31').status, 0, actual)
  }
})

test('GLIBC ceiling rejects newer and malformed versions', () => {
  for (const actual of ['2.31.1', '2.32', '2.39']) {
    assert.equal(versionAtMost(actual, '2.31').status, 1, actual)
  }
  for (const actual of ['', '2.x', 'GLIBC_2.31']) {
    assert.equal(versionAtMost(actual, '2.31').status, 2, actual)
  }
})

test('GLIBC symbol extraction compares every numeric component', () => {
  const result = spawnSync(
    'sh',
    ['-c', '. "$1"; unpeel_highest_glibc_version', 'sh', helper],
    {
      encoding: 'utf8',
      input: [
        '0000 (GLIBC_2.2.5) memcpy',
        '0000 (GLIBC_2.9) pipe2',
        '0000 (GLIBC_2.31) pthread_clockjoin_np',
        'not a version',
        '0000 (GLIBC_2.29) getrandom'
      ].join('\n')
    }
  )
  assert.equal(result.status, 0)
  assert.equal(result.stdout.trim(), '2.31')
})
