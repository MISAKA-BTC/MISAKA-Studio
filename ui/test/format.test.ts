import assert from 'node:assert/strict'
import { test } from 'node:test'
import { tokens } from '../src/lib/format.ts'

test('context lengths read the way windows are sized, from the floor class to 2M', () => {
  assert.equal(tokens(12), '12')
  assert.equal(tokens(512), '512')
  assert.equal(tokens(4096), '4K')
  assert.equal(tokens(32_768), '32K')
  assert.equal(tokens(262_144), '256K')
  assert.equal(tokens(2_097_152), '2M', 'not 2048K')
  assert.equal(tokens(3_000_000), '2.9M')
  assert.equal(tokens(null), '—')
})
