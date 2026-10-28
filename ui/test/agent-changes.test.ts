import assert from 'node:assert/strict'
import { test } from 'node:test'
import { diskState, revertChange, revertNote } from '../src/lib/agent/changes.ts'
import { fakeWorkspace } from '../src/lib/agent/fakeWorkspace.ts'
import type { FileChange } from '../src/lib/types.ts'

const change = (patch: Partial<FileChange>): FileChange => ({ path: 'a.txt', original: 'before\n', current: 'after\n', updatedAt: 1, ...patch })

test('a file as the agent left it is put back as it was', async () => {
  const api = fakeWorkspace({ 'a.txt': 'after\n' })
  assert.deepEqual(await revertChange(api, '/fake', change({})), { ok: true })
  assert.equal(api.files.get('a.txt'), 'before\n')
})

test('a file the agent created is removed', async () => {
  const api = fakeWorkspace({ 'new.txt': 'made\n' })
  assert.deepEqual(await revertChange(api, '/fake', change({ path: 'new.txt', original: null, current: 'made\n' })), { ok: true })
  assert.equal(api.files.has('new.txt'), false)
})

test('a file edited since the agent left it is not overwritten — unless the person says so', async () => {
  const api = fakeWorkspace({ 'a.txt': 'after\nmy own line\n' })
  const refused = await revertChange(api, '/fake', change({}))
  assert.deepEqual(refused, { ok: false, conflict: { state: 'edited', content: 'after\nmy own line\n' } })
  assert.equal(api.files.get('a.txt'), 'after\nmy own line\n', 'the person’s edit is still there')
  assert.ok(!api.calls.some((c) => c.method === 'write' || c.method === 'remove'))
  assert.deepEqual(await revertChange(api, '/fake', change({}), true), { ok: true })
  assert.equal(api.files.get('a.txt'), 'before\n')
})

test('a created file edited since is not removed without asking', async () => {
  const api = fakeWorkspace({ 'new.txt': 'made\nand then some\n' })
  const refused = await revertChange(api, '/fake', change({ path: 'new.txt', original: null, current: 'made\n' }))
  assert.equal(refused.ok, false)
  assert.equal(api.files.has('new.txt'), true)
})

test('a file gone from disk is reported, and written back when confirmed', async () => {
  const api = fakeWorkspace({})
  assert.deepEqual(await diskState(api, '/fake', change({})), { state: 'missing' })
  assert.deepEqual(await revertChange(api, '/fake', change({})), { ok: false, conflict: { state: 'missing' } })
  assert.deepEqual(await revertChange(api, '/fake', change({}), true), { ok: true })
  assert.equal(api.files.get('a.txt'), 'before\n')
})

test('an untracked change cannot be reverted, and says why', async () => {
  const result = await revertChange(fakeWorkspace({ 'big.bin': 'x' }), '/fake', change({ path: 'big.bin', original: null, current: null, untracked: true }))
  assert.equal(result.ok, false)
  assert.match((result as { error: string }).error, /too large to keep a copy of/)
})

test('the note a revert leaves tells the agent to read the file again', () => {
  assert.equal(revertNote(['src/a.ts']), 'Note: the user reverted your changes to `src/a.ts`. It is back to how it was before you changed it; read it again before editing it.')
  assert.match(revertNote(['a', 'b']), /changes to `a`, `b`\. They are back/)
})
